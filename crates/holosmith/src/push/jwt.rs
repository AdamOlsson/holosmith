//! The VAPID JWT (RFC 8292) and the little DER writer the two `openssl` children need (HS-066, ADR-0071).
//!
//! A Web Push request carries `Authorization: vapid t=<jwt>, k=<public key>`. The JWT is ES256 over
//! `{typ,alg}.{aud,exp,sub}`, and **the signature is not written here**: P-256 scalar multiplication is ~400
//! lines in which a bug is a `403` that looks like a configuration mistake, and `openssl` is already a host
//! dependency (the deploy generates the pair with it, the key proxy and this story's sender are `s_client`
//! children). So this module's real work is the *arrows*: the machine's base64url pair turned back into the
//! 121-octet SEC1 DER that [`crate::vapid::from_der`] parses, the 91-octet SubjectPublicKeyInfo that `openssl`
//! verifies with, and a DER `SEQUENCE{INTEGER r, INTEGER s}` turned into the 64-octet `r‖s` JOSE wants.
//!
//! Three rules it keeps:
//!
//! - **`aud` is the endpoint's origin**, not the endpoint: `https://host[:port]` and nothing else
//!   ([`origin`]). A push service compares the two, so a URL with a path in it is a token every service refuses.
//! - **`exp` is twelve hours away** ([`EXP_SECONDS`]). RFC 8292's ceiling is 24 h, and the value is *because* the
//!   ceiling exists: a token that never expires is one a leaked log line keeps working for ever, and a token
//!   shorter than a shift would expire mid-turn. Twelve is a shift's worth.
//! - **The key material is a `0600` file inside a `0700` directory that is removed on drop** ([`Temp`]): the
//!   private scalar is what a signature proves and nothing else should read, so it is created with its mode
//!   ([`std::os::unix::fs::OpenOptionsExt::mode`]) rather than chmod-ed afterwards — there is no window between
//!   `create` and `chmod` in which a file is briefly world-readable.
//!
//! [`Temp`] is one type used by three modules (`jwt`, `encrypt` and indirectly `send`), and it lives here because
//! the JWT is the first thing a send builds.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::vapid;
use crate::vapid::Pair;

/// How long a token is good for: RFC 8292's ceiling is 24 h, and twelve is a shift's worth.
pub const EXP_SECONDS: i64 = 12 * 3600;

/// The subdirectory of the data root the per-send directories live under.
const TEMP_DIR: &str = "push/tmp";

/// The `alg` this build signs with, and the `typ` RFC 7519 recommends.
const ALG: &str = "ES256";
const TYP: &str = "JWT";

/// The JWT header, as a struct so the two keys serialise in this order and a test can pin the bytes.
#[derive(serde::Serialize)]
struct Header {
    typ: &'static str,
    alg: &'static str,
}

/// The JWT claims: RFC 8292 §2's three.
#[derive(serde::Serialize)]
struct Claims<'a> {
    aud: &'a str,
    exp: i64,
    sub: &'a str,
}

/// The endpoint's origin — `https://host[:port]` — or `None` when the endpoint is not a URL this can be the
/// audience of.
///
/// "And nothing else" is the whole rule: no path, no query, no fragment, no userinfo, no whitespace. A push
/// endpoint is `https` (a service worker's `pushManager.subscribe` requires it, and this build has no way to use a
/// plaintext one), so an `http://` endpoint is refused rather than upgraded — silently rewriting a client's value
/// would hide the mistake until a push service answered `403`.
pub fn origin(endpoint: &str) -> Option<String> {
    if endpoint.is_empty()
        || endpoint.contains(char::is_whitespace)
        || endpoint.contains('#')
        || endpoint.contains('@')
    {
        return None;
    }
    let rest = endpoint.strip_prefix("https://")?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if !is_authority(authority) {
        return None;
    }
    Some(format!("https://{authority}"))
}

/// Whether `authority` is `host[:port]`: a non-empty host, an optional numeric port, and no userinfo.
///
/// An IPv6 literal is `[::1]` and its colons are the host's, which is why the bracketed form is taken first — the
/// one place a naive `split_once(':')` would call the host `[` and the port `:1]`.
fn is_authority(authority: &str) -> bool {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, after)) = rest.split_once(']') else {
            return false;
        };
        let port = if after.is_empty() {
            None
        } else {
            match after.strip_prefix(':') {
                Some(port) => Some(port),
                None => return false,
            }
        };
        (host, port)
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() || host.contains('/') {
        return false;
    }
    match port {
        None => true,
        Some(port) => !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

/// The machine's pair as the 121-octet SEC1 DER that [`crate::vapid::from_der`] parses — the writer to that
/// reader, and the input `openssl dgst -sign … -keyform DER` needs.
///
/// The test that round-trips `from_der(sec1(pair)) == pair` is what proves the two agree, which is the only
/// thing that makes "this DER is the pair" a measurement rather than a hope.
pub fn sec1(pair: &Pair) -> Result<Vec<u8>, String> {
    let scalar = vapid::unbase64url(&pair.private_key)
        .ok_or_else(|| "the VAPID private key is not unpadded base64url".to_string())?;
    let point = vapid::unbase64url(&pair.public_key)
        .ok_or_else(|| "the VAPID public key is not unpadded base64url".to_string())?;
    if scalar.len() != 32 {
        return Err(format!(
            "the VAPID private key is {} octets, not the 32 a P-256 scalar is",
            scalar.len()
        ));
    }
    if point.len() != 65 || point[0] != 0x04 {
        return Err(format!(
            "the VAPID public key is {} octets, not the 65 an uncompressed P-256 point is",
            point.len()
        ));
    }
    let mut der = vec![0x30, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];
    der.extend_from_slice(&scalar);
    // The named curve: `prime256v1`'s OID as context [0].
    der.extend_from_slice(&[
        0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
    ]);
    // The public key as context [1]'s BIT STRING: `0x00` unused-bits byte, then the point.
    der.extend_from_slice(&[0xa1, 0x44, 0x03, 0x42, 0x00]);
    der.extend_from_slice(&point);
    Ok(der)
}

/// The 91-octet SubjectPublicKeyInfo for a 65-octet point: what `openssl dgst -verify … -keyform DER` reads.
pub fn spki(point: &[u8]) -> Result<Vec<u8>, String> {
    if point.len() != 65 || point[0] != 0x04 {
        return Err(format!(
            "a P-256 public key is 65 octets and starts 0x04, and this one is {} octets starting 0x{:02x}",
            point.len(),
            point.first().copied().unwrap_or(0)
        ));
    }
    let mut der = vec![
        0x30, 0x59, // SubjectPublicKeyInfo
        0x30, 0x13, // AlgorithmIdentifier
        0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, // id-ecPublicKey
        0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, // prime256v1
        0x03, 0x42, 0x00, // BIT STRING, no unused bits
    ];
    der.extend_from_slice(point);
    Ok(der)
}

/// The 64-octet `r‖s` JOSE wants, from the DER `SEQUENCE{INTEGER r, INTEGER s}` `openssl dgst -sign` answers.
///
/// Each integer is left-padded to 32 octets and its DER sign padding dropped: a real `r` with its top bit set is
/// 33 octets on the wire (`0x00` then the 32), and an integer *longer* than 32 after that padding is not a P-256
/// scalar — a refusal rather than a truncation, because a truncated `r` is a signature that verifies nowhere and
/// looks like a wrong key.
pub fn raw_signature(der: &[u8]) -> Result<[u8; 64], String> {
    let (sequence, after) = element(der, 0x30, "the signature as a SEQUENCE (0x30)")?;
    if !after.is_empty() {
        return Err(refused("nothing after the SEQUENCE"));
    }
    let (r, after) = element(sequence, 0x02, "`r` as an INTEGER (0x02)")?;
    let (s, after) = element(after, 0x02, "`s` as an INTEGER (0x02)")?;
    if !after.is_empty() {
        return Err(refused("nothing after `s`"));
    }
    let mut signature = [0_u8; 64];
    signature[..32].copy_from_slice(&fixed(r)?);
    signature[32..].copy_from_slice(&fixed(s)?);
    Ok(signature)
}

/// [`raw_signature`]'s inverse, for [`verify`]'s child: `r‖s` back into a DER `SEQUENCE` of two INTEGERs.
fn der_signature(signature: &[u8; 64]) -> Vec<u8> {
    fn integer(bytes: &[u8]) -> Vec<u8> {
        let mut trimmed = bytes;
        while trimmed.len() > 1 && trimmed[0] == 0 {
            trimmed = &trimmed[1..];
        }
        // DER's INTEGER is signed, so a value with its top bit set carries a leading zero.
        let pad = usize::from(trimmed[0] & 0x80 != 0);
        let mut out = vec![0x02, (trimmed.len() + pad) as u8];
        if pad == 1 {
            out.push(0);
        }
        out.extend_from_slice(trimmed);
        out
    }
    let (r, s) = signature.split_at(32);
    let body = [integer(r), integer(s)].concat();
    let mut der = vec![0x30, body.len() as u8];
    der.extend_from_slice(&body);
    der
}

/// One DER element's content, and what follows it. Short-form lengths only, for the reason
/// [`crate::vapid::from_der`]'s reader gives: a long-form length means the bytes are another structure.
fn element<'a>(bytes: &'a [u8], tag: u8, what: &str) -> Result<(&'a [u8], &'a [u8]), String> {
    let found = *bytes
        .first()
        .ok_or_else(|| refused(&format!("{what} where the value ended")))?;
    if found != tag {
        return Err(refused(&format!(
            "{what}, not the 0x{found:02x} that is there"
        )));
    }
    let length = *bytes
        .get(1)
        .ok_or_else(|| refused(&format!("{what}, and then its length")))?;
    if length >= 0x80 {
        return Err(refused(&format!(
            "{what} with a short-form length, where a long-form one is here"
        )));
    }
    let start = 2;
    let end = start + usize::from(length);
    if end > bytes.len() {
        return Err(refused(&format!(
            "{what} carrying {length} octets, where the value ends early"
        )));
    }
    Ok((&bytes[start..end], &bytes[end..]))
}

/// An INTEGER's octets as a 32-octet big-endian scalar, sign padding dropped.
fn fixed(integer: &[u8]) -> Result<[u8; 32], String> {
    let mut bytes = integer;
    while bytes.len() > 1 && bytes[0] == 0 {
        bytes = &bytes[1..];
    }
    if bytes.len() > 32 {
        return Err(refused(&format!(
            "a P-256 scalar of at most 32 octets, where this integer is {}",
            bytes.len()
        )));
    }
    let mut out = [0_u8; 32];
    out[32 - bytes.len()..].copy_from_slice(bytes);
    Ok(out)
}

/// The one sentence a refusal about the DER uses, so every arm says which producer was expected.
fn refused(what: &str) -> String {
    format!(
        "`openssl dgst -sha256 -sign <key> -keyform DER` answers a DER SEQUENCE of two INTEGERs, and this is not \
         one: {what} was expected"
    )
}

/// A signed VAPID token for `audience`: `<header>.<claims>.<r‖s>`, base64url and unpadded.
///
/// The signing input goes to `openssl` on stdin and the pair goes into `tmp` as SEC1 DER, so no secret is ever an
/// argument in a process list (the one place a `ps` would show it).
pub fn token(
    pair: &Pair,
    audience: &str,
    sub: &str,
    now: i64,
    openssl: &str,
    tmp: &Temp,
) -> Result<String, String> {
    let header = serde_json::to_vec(&Header { typ: TYP, alg: ALG })
        .map_err(|error| format!("the JWT header could not be serialised ({error})"))?;
    let claims = serde_json::to_vec(&Claims {
        aud: audience,
        exp: now.saturating_add(EXP_SECONDS),
        sub,
    })
    .map_err(|error| format!("the JWT claims could not be serialised ({error})"))?;
    let signing_input = format!(
        "{}.{}",
        vapid::base64url(&header),
        vapid::base64url(&claims)
    );
    let key = tmp.file("vapid.der", &sec1(pair)?)?;
    let signature = run(
        openssl,
        &[
            "dgst",
            "-sha256",
            "-sign",
            &key.display().to_string(),
            "-keyform",
            "DER",
        ],
        signing_input.as_bytes(),
    )?;
    Ok(format!(
        "{signing_input}.{}",
        vapid::base64url(&raw_signature(&signature)?)
    ))
}

/// Whether `signature` is this pair's ES256 signature over `signing_input` — a **second** `openssl` child, and the
/// reason [`token`]'s claim is a measurement.
///
/// Public because the crate's tests live outside it (`tests/push.rs`), which is the same reason `Deleter::remove`
/// and `Turn::run` are: "we signed with the machine's pair" is a sentence only a verifier can check, and this
/// build's sender never calls it.
pub fn verify(
    public_key: &str,
    signing_input: &str,
    signature: &str,
    openssl: &str,
    tmp: &Temp,
) -> Result<(), String> {
    let point = vapid::unbase64url(public_key)
        .ok_or_else(|| "the VAPID public key is not unpadded base64url".to_string())?;
    let key = tmp.file("verify-key.der", &spki(&point)?)?;
    let raw = vapid::unbase64url(signature)
        .ok_or_else(|| "the signature is not unpadded base64url".to_string())?;
    let signature: [u8; 64] = raw.as_slice().try_into().map_err(|_| {
        format!(
            "an ES256 signature is 64 octets, and this one is {}",
            raw.len()
        )
    })?;
    let file = tmp.file("verify-signature.der", &der_signature(&signature))?;
    let answer = run(
        openssl,
        &[
            "dgst",
            "-sha256",
            "-verify",
            &key.display().to_string(),
            "-keyform",
            "DER",
            "-signature",
            &file.display().to_string(),
        ],
        signing_input.as_bytes(),
    )?;
    if answer.starts_with(b"Verified OK") {
        return Ok(());
    }
    Err(format!(
        "the signature did not verify: {}",
        String::from_utf8_lossy(&answer).trim()
    ))
}

/// One `openssl` child with `input` on stdin, answering its stdout — or a sentence naming the program, what it
/// said, and that nothing was sent.
fn run(openssl: &str, arguments: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new(openssl)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("`{openssl}` could not be run ({error})"))?;
    // The child's stdin is closed before its answer is read: `wait_with_output` waits for the process, and a child
    // still holding an open stdin at EOF is a child that never ends. The block is the close.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("`{openssl}` did not accept its input ({error})"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("`{openssl}` could not be waited for ({error})"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "`{openssl} {}` failed: {said}",
            arguments.join(" ")
        ));
    }
    Ok(output.stdout)
}

/// A `0700` directory under `<data root>/push/tmp/`, with `0600` files inside it, removed on drop.
///
/// Everything a send needs a file for goes here — this pair's SEC1 DER, and RFC 8291's application-server key —
/// and nothing outlives the send. The window is the send's: a crash between [`Temp::new`] and its `Drop` leaves a
/// directory holding one dead key pair under a data root only this machine's user can read, which is the
/// acceptable failure (the alternative, one shared directory, would mean two concurrent sends racing for the same
/// path).
///
/// The name carries the process id and a counter, because two sends of one batch are two of these and nothing in
/// this crate generates a random name.
pub struct Temp {
    dir: PathBuf,
}

/// What makes two `Temp`s in one process differ.
static NEXT: AtomicU64 = AtomicU64::new(1);

impl Temp {
    /// A fresh directory under [`TEMP_DIR`], created with mode 0700.
    pub fn new(data_root: &Path) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        let parent = data_root.join(TEMP_DIR);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)
            .map_err(|error| format!("{} could not be created ({error})", parent.display()))?;
        let name = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = parent.join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|error| format!("{} could not be created ({error})", dir.display()))?;
        Ok(Self { dir })
    }

    /// Write `bytes` as `<name>` inside the directory, at mode 0600, and answer its path.
    ///
    /// The mode is set by the `open` call rather than by a `chmod` afterwards: a `chmod` leaves the file readable for
    /// as long as it takes to run, which for a private key is the whole problem. A name written twice in one directory
    /// is replaced: the directory is one send's, so this call is its only writer and a second write is the same
    /// secret, not somebody else's.
    pub fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.dir.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| format!("{} could not be created ({error})", path.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("{} could not be written ({error})", path.display()))?;
        Ok(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 8291 §5 vector's application-server pair, as `vapid.toml` would hold it.
    fn pair() -> Pair {
        Pair {
            public_key: "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
                .to_string(),
            private_key: "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw".to_string(),
        }
    }

    /// A scratch data root, removed when the test ends.
    fn data_root(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("holosmith-push-jwt-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        dir
    }

    #[test]
    fn an_origin_is_the_scheme_host_and_port_and_nothing_else() {
        for (endpoint, wanted) in [
            (
                "https://push.example.com/one/two?three=four",
                Some("https://push.example.com"),
            ),
            (
                "https://push.example.com:8443/x",
                Some("https://push.example.com:8443"),
            ),
            ("https://push.example.com", Some("https://push.example.com")),
            (
                "https://push.example.com?query",
                Some("https://push.example.com"),
            ),
            ("https://[::1]:8443/x", Some("https://[::1]:8443")),
            ("https://[::1]/x", Some("https://[::1]")),
        ] {
            assert_eq!(origin(endpoint).as_deref(), wanted, "{endpoint}");
        }
        for refused in [
            // Not https.
            "http://push.example.com/x",
            "HTTPS://push.example.com/x",
            // No host.
            "https://",
            "https:///path",
            "https://:8443/x",
            // A fragment is not an origin.
            "https://push.example.com#fragment",
            // Whitespace is a header injection waiting to happen, and never a URL.
            "https://push.example.com /x",
            " https://push.example.com",
            // Userinfo is not the authority.
            "https://user:pass@push.example.com/x",
            // A port is digits.
            "https://push.example.com:eight/x",
            "https://push.example.com:/x",
            // Not a URL at all.
            "",
            "push.example.com",
            "mailto:holosmith@localhost",
            "https://[::1/x",
        ] {
            assert_eq!(origin(refused), None, "{refused}");
        }
    }

    #[test]
    fn sec1_is_the_shape_from_der_walks() {
        let pair = pair();
        let der = sec1(&pair).expect("the vector's pair is a pair");
        // 121 octets, the length `openssl ecparam -name prime256v1 -genkey -noout -outform DER` writes.
        assert_eq!(der.len(), 121);
        // The writer and the reader agree: this is the proof that the two directions are one encoding.
        assert_eq!(vapid::from_der(&der), Ok(pair.clone()));
        // And a pair whose fields are the wrong length is a sentence rather than a key.
        let mut short = pair.clone();
        short.private_key = vapid::base64url(&[0x11; 31]);
        assert!(sec1(&short).is_err());
        let mut compressed = pair;
        compressed.public_key = vapid::base64url(&[0x02; 65]);
        assert!(sec1(&compressed).is_err());
    }

    #[test]
    fn spki_is_91_octets_and_refuses_a_point_that_is_not_one() {
        let point = vapid::unbase64url(&pair().public_key).expect("the vector's key is base64url");
        let der = spki(&point).expect("a 65-octet point");
        assert_eq!(der.len(), 91);
        assert_eq!(&der[..4], &[0x30, 0x59, 0x30, 0x13]);
        assert_eq!(&der[der.len() - 65..], &point[..]);
        assert!(spki(&point[..64]).is_err());
    }

    #[test]
    fn a_real_signature_walks_to_64_octets() {
        // A real `openssl` signature over a real signing input, and the round trip back: `raw_signature` then
        // `der_signature` is the same DER, which is what makes `verify`'s child honest.
        let root = data_root("signature");
        let tmp = Temp::new(&root).expect("a temp directory");
        let key = tmp
            .file("key.der", &sec1(&pair()).expect("a DER"))
            .expect("the key file");
        let input = b"the-signing-input";
        let der = run(
            "openssl",
            &[
                "dgst",
                "-sha256",
                "-sign",
                &key.display().to_string(),
                "-keyform",
                "DER",
            ],
            input,
        )
        .expect("openssl signs");
        let raw = raw_signature(&der).expect("openssl answers a DER signature");
        assert_eq!(der_signature(&raw), der, "the walk and the writer disagree");
        // A signature that is not a SEQUENCE of two INTEGERs is three sentences, not a panic.
        assert!(raw_signature(&[]).is_err());
        assert!(raw_signature(&[0x02, 0x01, 0x01]).is_err());
        // A 33-octet integer after its sign padding is not a P-256 scalar.
        let mut long = vec![0x30, 0x26, 0x02, 0x21];
        long.extend_from_slice(&[0x01; 33]);
        long.extend_from_slice(&[0x02, 0x01, 0x01]);
        let sentence = raw_signature(&long).expect_err("a 33-octet r is not a scalar");
        assert!(sentence.contains("at most 32"), "{sentence}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_integer_with_its_top_bit_set_keeps_one_sign_octet() {
        // P-256's `r` is 32 octets, so a real one with the top bit set is 33 on the wire. The inverse has to
        // reintroduce exactly one `0x00`, not two and not none.
        let mut signature = [0x7f_u8; 64];
        signature[0] = 0x80;
        signature[32] = 0x01;
        let der = der_signature(&signature);
        // `r` keeps one sign octet and `s` needs none, so the SEQUENCE is 69 octets: 35 for `r`, 34 for `s`.
        assert_eq!(&der[..4], &[0x30, 0x45, 0x02, 0x21]);
        assert_eq!(der[4], 0x00);
        assert_eq!(raw_signature(&der).expect("it walks back"), signature);
    }

    #[test]
    fn a_token_is_three_parts_and_its_signature_verifies() {
        let root = data_root("token");
        let tmp = Temp::new(&root).expect("a temp directory");
        let pair = pair();
        let token = token(
            &pair,
            "https://push.example",
            "mailto:me@example",
            1_760_000_000,
            "openssl",
            &tmp,
        )
        .expect("the machine can sign");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "{token}");
        assert_eq!(
            vapid::unbase64url(parts[0]).expect("base64url"),
            br#"{"typ":"JWT","alg":"ES256"}"#
        );
        assert_eq!(
            vapid::unbase64url(parts[1]).expect("base64url"),
            format!(
                r#"{{"aud":"https://push.example","exp":{},"sub":"mailto:me@example"}}"#,
                1_760_000_000 + EXP_SECONDS
            )
            .into_bytes()
        );
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        assert_eq!(
            verify(&pair.public_key, &signing_input, parts[2], "openssl", &tmp),
            Ok(())
        );
        // A tampered signing input is a refusal, which is what makes the verification a measurement.
        assert!(
            verify(
                &pair.public_key,
                &format!("{signing_input}x"),
                parts[2],
                "openssl",
                &tmp
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_temp_directory_is_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = data_root("temp");
        let path = {
            let tmp = Temp::new(&root).expect("a temp directory");
            let file = tmp.file("secret", b"the scalar").expect("the file");
            let mode = std::fs::metadata(&file)
                .expect("the file exists")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the file is not 0600");
            let dir = file
                .parent()
                .expect("the file is inside the directory")
                .to_path_buf();
            let mode = std::fs::metadata(&dir)
                .expect("the directory exists")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "the directory is not 0700");
            dir
        };
        assert!(!path.exists(), "the directory outlived its Temp");
        // The parent stays: it is the data root's, not one send's.
        assert!(root.join(TEMP_DIR).is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }
}
