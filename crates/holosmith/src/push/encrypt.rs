//! RFC 8291's `aes128gcm` message: one ECDH child, three HMACs, and the body a push service delivers (HS-066).
//!
//! The algorithm is the RFC's §3.4 and it is order-sensitive in two places that a plausible rewrite gets wrong:
//! the `key_info` string is `"WebPush: info" ‖ 0x00 ‖ ua_public ‖ as_public` and the trailing `0x00` is a
//! **separator**, not padding; and the record's plaintext has a `0x02` appended *before* it is sealed, which is
//! the padding delimiter a receiver strips. Both are in the golden test below, which is RFC 8291 §5's complete
//! example byte for byte.
//!
//! The pieces:
//!
//! ```text
//! ecdh_secret = ECDH(as_private, ua_public)                       // an `openssl pkeyutl -derive` child
//! PRK_key     = HMAC-SHA-256(auth_secret, ecdh_secret)
//! key_info    = "WebPush: info" ‖ 0x00 ‖ ua_public ‖ as_public
//! IKM         = HMAC-SHA-256(PRK_key, key_info ‖ 0x01)            // 32 octets
//! PRK         = HMAC-SHA-256(salt, IKM)
//! CEK         = HMAC-SHA-256(PRK, "Content-Encoding: aes128gcm" ‖ 0x00 ‖ 0x01)[0..16]
//! NONCE       = HMAC-SHA-256(PRK, "Content-Encoding: nonce"     ‖ 0x00 ‖ 0x01)[0..12]
//! header      = salt(16) ‖ rs(4, big-endian) ‖ 0x41 ‖ as_public(65)      // 86 octets
//! body        = header ‖ AES-128-GCM(CEK, NONCE, plaintext ‖ 0x02)
//! ```
//!
//! **ECDH is `openssl`'s**, for the reason the JWT's signature is ([`crate::push::jwt`]): `openssl pkeyutl
//! -derive` is a measured host command, and a second hand-written P-256 would be the same ~400 lines with a
//! second set of mistakes. Everything around it — the three HMACs, the header, the sealing — is
//! [`crate::push::crypto`], which is pinned by published vectors.
//!
//! **The salt is fresh per message and comes from `/dev/urandom`**, not from a child: a message is four children
//! already (`ecparam`, `pkeyutl`, `dgst`, `s_client` — the plan's D6), and a fifth for sixteen octets would be
//! the one child whose output is not a measurement. A machine with no `/dev/urandom` gets an error and no send,
//! because a salt an attacker could guess makes two messages share a keystream, which is worse than silence.

use crate::push::crypto;
use crate::push::jwt::{self, Temp};
use crate::vapid;

/// A salt's length: 16 octets, RFC 8291 §4's first header field.
pub const SALT: usize = 16;

/// The `rs` header field: the record size, and it must exceed the plaintext plus its delimiter and tag
/// (RFC 8291 §4). A notification is `{host, session_id, kind}` — tens of octets — so one record always carries it.
pub const RECORD: u32 = 4096;

/// What a browser's `pushManager.subscribe()` produced, as the table stores it: the endpoint and the two keys,
/// base64url decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub endpoint: String,
    /// The user agent's public key: 65 octets, uncompressed, starting `0x04`.
    pub p256dh: Vec<u8>,
    /// The authentication secret: 16 octets.
    pub auth: Vec<u8>,
}

/// A fresh application-server key pair: its 65-octet point, and its private half as the SEC1 DER the ECDH child
/// reads.
///
/// Not a [`vapid::Pair`]: the private half never leaves this module as base64url, and a value that cannot be
/// mistaken for the machine's VAPID pair is one that cannot be passed where that pair is meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ephemeral {
    pub point: Vec<u8>,
    pub der: Vec<u8>,
}

/// A fresh application-server key pair, from `openssl ecparam -name prime256v1 -genkey -noout -outform DER` —
/// the command the deploy already generates the machine's pair with — walked by [`crate::vapid::from_der`].
pub fn ephemeral(openssl: &str) -> Result<Ephemeral, String> {
    let output = std::process::Command::new(openssl)
        .args([
            "ecparam",
            "-name",
            "prime256v1",
            "-genkey",
            "-noout",
            "-outform",
            "DER",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("`{openssl}` could not be run ({error})"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("`{openssl} ecparam` failed: {said}"));
    }
    let pair = vapid::from_der(&output.stdout)?;
    let point = vapid::unbase64url(&pair.public_key)
        .ok_or_else(|| "the fresh key's point is not base64url".to_string())?;
    Ok(Ephemeral {
        point,
        der: jwt::sec1(&pair)?,
    })
}

/// A salt for one message, from `/dev/urandom`.
pub fn salt() -> Result<[u8; SALT], String> {
    let mut salt = [0_u8; SALT];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut salt))
        .map_err(|error| {
            format!(
                "a message salt could not be read from /dev/urandom ({error}), so nothing was sent"
            )
        })?;
    Ok(salt)
}

/// The three HMACs between the ECDH secret and the record's two keys, named so a failure names the step.
///
/// Separate from [`seal`] because the RFC publishes these intermediates (`/tmp/exp/rfc8291.mjs`'s output, and the
/// RFC's appendix): one vector then witnesses five steps, and a broken one says *which*.
struct Keys {
    /// Read only by the golden test's five witnesses: the RFC publishes these three, and a failure that names the
    /// step is worth more than a failure that names the whole algorithm.
    #[allow(dead_code)]
    prk_key: [u8; 32],
    #[allow(dead_code)]
    ikm: [u8; 32],
    #[allow(dead_code)]
    prk: [u8; 32],
    cek: [u8; 16],
    nonce: [u8; 12],
}

/// RFC 8291 §3.3/§3.4's key derivation, from the raw ECDH secret.
fn keys(
    ecdh_secret: &[u8],
    auth_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; SALT],
) -> Keys {
    let prk_key = crypto::hmac_sha256(auth_secret, &[ecdh_secret]);
    let mut key_info = Vec::with_capacity(14 + ua_public.len() + as_public.len());
    key_info.extend_from_slice(b"WebPush: info");
    key_info.push(0);
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let ikm = crypto::hmac_sha256(&prk_key, &[&key_info, &[0x01]]);
    let prk = crypto::hmac_sha256(salt, &[&ikm]);
    let cek_digest = crypto::hmac_sha256(&prk, &[b"Content-Encoding: aes128gcm", &[0x00], &[0x01]]);
    let nonce_digest = crypto::hmac_sha256(&prk, &[b"Content-Encoding: nonce", &[0x00], &[0x01]]);
    let mut cek = [0_u8; 16];
    cek.copy_from_slice(&cek_digest[..16]);
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&nonce_digest[..12]);
    Keys {
        prk_key,
        ikm,
        prk,
        cek,
        nonce,
    }
}

/// The `aes128gcm` body of one push message: `header ‖ AES-128-GCM(plaintext ‖ 0x02)`.
///
/// The delimiter is appended **here** and not by the caller, because RFC 8291 §4 makes it part of the record: a
/// body without it is one every receiver rejects, and a caller that could forget it is a caller that will.
pub fn seal(
    subscription: &Subscription,
    plaintext: &[u8],
    as_pair: &Ephemeral,
    salt: &[u8; SALT],
    openssl: &str,
    tmp: &Temp,
) -> Result<Vec<u8>, String> {
    let ua_public = subscription.p256dh.as_slice();
    if ua_public.len() != 65 || ua_public[0] != 0x04 {
        return Err(format!(
            "the subscription's p256dh is {} octets, not the 65 an uncompressed P-256 point is",
            ua_public.len()
        ));
    }
    if subscription.auth.len() != 16 {
        return Err(format!(
            "the subscription's auth is {} octets, not the 16 RFC 8291 §3.2 asks for",
            subscription.auth.len()
        ));
    }
    let ecdh_secret = derive(as_pair, ua_public, openssl, tmp)?;
    Ok(body(
        &keys(
            &ecdh_secret,
            &subscription.auth,
            ua_public,
            &as_pair.point,
            salt,
        ),
        plaintext,
        as_pair,
        salt,
    ))
}

/// The record, once the keys are derived: the header then the sealed plaintext and its delimiter.
fn body(keys: &Keys, plaintext: &[u8], as_pair: &Ephemeral, salt: &[u8; SALT]) -> Vec<u8> {
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    let mut body = Vec::with_capacity(86 + record.len() + 16);
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD.to_be_bytes());
    // `idlen`, then the key id — which for `aes128gcm` is the application server's public key.
    body.push(0x41);
    body.extend_from_slice(&as_pair.point);
    body.extend_from_slice(&crypto::seal(&keys.cek, &keys.nonce, &record));
    body
}

/// The raw ECDH secret: an `openssl pkeyutl -derive` child with the application server's SEC1 private key and the
/// user agent's public key as a SubjectPublicKeyInfo.
fn derive(
    as_pair: &Ephemeral,
    ua_public: &[u8],
    openssl: &str,
    tmp: &Temp,
) -> Result<Vec<u8>, String> {
    let private = tmp.file("as.der", &as_pair.der)?;
    let public = tmp.file("ua.spki", &jwt::spki(ua_public)?)?;
    let output = std::process::Command::new(openssl)
        .args([
            "pkeyutl",
            "-derive",
            "-inkey",
            &private.display().to_string(),
            "-keyform",
            "DER",
            "-peerkey",
            &public.display().to_string(),
            "-peerform",
            "DER",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("`{openssl}` could not be run ({error})"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("`{openssl} pkeyutl -derive` failed: {said}"));
    }
    if output.stdout.len() != 32 {
        return Err(format!(
            "`{openssl} pkeyutl -derive` answered {} octets, not the 32 a P-256 secret is",
            output.stdout.len()
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 8291 §5 vector's four values, base64url as the RFC writes them.
    const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const RFC_SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const AUTH_SECRET: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const RFC_PLAINTEXT: &str = "When I grow up, I want to be a watermelon";
    const RFC_BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

    /// The RFC's application-server pair, as an [`Ephemeral`].
    fn vector() -> Ephemeral {
        let pair = vapid::Pair {
            public_key: AS_PUBLIC.to_string(),
            private_key: AS_PRIVATE.to_string(),
        };
        Ephemeral {
            point: vapid::unbase64url(&pair.public_key).expect("the RFC's point"),
            der: jwt::sec1(&pair).expect("the RFC's scalar is a key"),
        }
    }

    /// A scratch data root, removed when the test ends.
    fn data_root(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "holosmith-push-encrypt-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        dir
    }

    #[test]
    fn the_rfc_8291_example_matches_byte_for_byte_and_names_each_step() {
        let root = data_root("vector");
        let tmp = Temp::new(&root).expect("a temp directory");
        let ua_public = vapid::unbase64url(UA_PUBLIC).expect("the RFC's point");
        let as_pair = vector();
        let salt: [u8; SALT] = vapid::unbase64url(RFC_SALT)
            .expect("the RFC's salt")
            .try_into()
            .expect("a 16-octet salt");
        let subscription = Subscription {
            endpoint: "https://push.example/one".to_string(),
            p256dh: ua_public.clone(),
            auth: vapid::unbase64url(AUTH_SECRET).expect("the RFC's auth secret"),
        };

        // One vector, five witnesses: the intermediates the RFC publishes, so a failure names the step rather
        // than the whole algorithm. The secret is `as_private`'s, which is what the child derives from the same
        // pair.
        let secret = derive(&as_pair, &ua_public, "openssl", &tmp).expect("openssl derives");
        assert_eq!(
            vapid::base64url(&secret),
            "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"
        );
        let keys = keys(
            &secret,
            &subscription.auth,
            &ua_public,
            &as_pair.point,
            &salt,
        );
        assert_eq!(
            vapid::base64url(&keys.prk_key),
            "Snr3JMxaHVDXHWJn5wdC52WjpCtd2EIEGBykDcZW32k"
        );
        assert_eq!(
            vapid::base64url(&keys.ikm),
            "S4lYMb_L0FxCeq0WhDx813KgSYqU26kOyzWUdsXYyrg"
        );
        assert_eq!(
            vapid::base64url(&keys.prk),
            "09_eUZGrsvxChDCGRCdkLiDXrReGOEVeSCdCcPBSJSc"
        );
        assert_eq!(vapid::base64url(&keys.cek), "oIhVW04MRdy2XN9CiKLxTg");
        assert_eq!(vapid::base64url(&keys.nonce), "4h_95klXJ5E_qnoN");

        // And the whole body, which is the four steps composed the way §4 composes them.
        let body = seal(
            &subscription,
            RFC_PLAINTEXT.as_bytes(),
            &as_pair,
            &salt,
            "openssl",
            &tmp,
        )
        .expect("the vector seals");
        // The RFC's own `Content-Length: 145` is an erratum: the body is 144 octets, and that is the statement
        // this length assertion makes.
        assert_eq!(body.len(), 144);
        assert_eq!(vapid::base64url(&body), RFC_BODY);

        // The header's three fields, read back out of the body: the salt, the record size and the key id.
        assert_eq!(&body[..16], &salt[..]);
        assert_eq!(&body[16..20], &RECORD.to_be_bytes());
        assert_eq!(body[20], 0x41);
        assert_eq!(&body[21..86], &as_pair.point[..]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_payload_is_three_keys_and_the_body_carries_none_of_the_plaintext() {
        // D10's promise, measured: the plaintext is a struct with exactly these three fields, so "the body says
        // nothing about a session's content" is structural rather than intended.
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            host: &'a str,
            session_id: &'a str,
            kind: &'a str,
        }
        let payload = Payload {
            host: crate::preview::HOST,
            session_id: "0123456789abcdef",
            kind: "turn_finished",
        };
        let text = serde_json::to_string(&payload).expect("three strings serialise");
        assert_eq!(
            text,
            r#"{"host":"local","session_id":"0123456789abcdef","kind":"turn_finished"}"#
        );

        let root = data_root("payload");
        let tmp = Temp::new(&root).expect("a temp directory");
        let ua_public = vapid::unbase64url(UA_PUBLIC).expect("the RFC's UA point");
        let as_pair = vector();
        let salt: [u8; SALT] = vapid::unbase64url(RFC_SALT)
            .expect("the RFC's salt")
            .try_into()
            .expect("a 16-octet salt");
        let subscription = Subscription {
            endpoint: "https://push.example/one".to_string(),
            p256dh: ua_public.clone(),
            auth: vapid::unbase64url(AUTH_SECRET).expect("the RFC's auth secret"),
        };
        let body = seal(
            &subscription,
            text.as_bytes(),
            &as_pair,
            &salt,
            "openssl",
            &tmp,
        )
        .expect("the payload seals");

        // The body carries none of the plaintext's bytes, and the receiver's own half recovers it: the ECDH
        // secret from the *user agent's* private key, then the same three HMACs, then `crypto::open`.
        for needle in [payload.host, payload.session_id, payload.kind] {
            assert!(
                !body
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "the sealed body carries {needle:?}"
            );
        }
        let ua_pair = vapid::Pair {
            public_key: UA_PUBLIC.to_string(),
            private_key: UA_PRIVATE.to_string(),
        };
        let ua_der = jwt::sec1(&ua_pair).expect("the UA's scalar is a key");
        let ecdh = derive(
            &Ephemeral {
                point: ua_public.clone(),
                der: ua_der,
            },
            &as_pair.point,
            "openssl",
            &tmp,
        )
        .expect("the receiver derives the same secret");
        let keys = keys(&ecdh, &subscription.auth, &ua_public, &as_pair.point, &salt);
        let opened = crate::push::crypto::open(&keys.cek, &keys.nonce, &body[86..])
            .expect("the receiver's tag verifies");
        let record = &opened[..opened.len() - 1];
        // RFC 8291 §4's delimiter is the last octet of the record's plaintext, and the receiver strips it.
        assert_eq!(*opened.last().expect("the record is not empty"), 0x02);
        let parsed: serde_json::Value =
            serde_json::from_slice(record).expect("the record is the payload's JSON");
        let mut names: Vec<&str> = parsed
            .as_object()
            .expect("the payload is an object")
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["host", "kind", "session_id"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_subscription_whose_keys_are_not_the_right_length_is_a_sentence() {
        let root = data_root("lengths");
        let tmp = Temp::new(&root).expect("a temp directory");
        let salt = [0_u8; SALT];
        let mut subscription = Subscription {
            endpoint: "https://push.example/one".to_string(),
            p256dh: vec![0x04; 64],
            auth: vec![0; 16],
        };
        let sentence = seal(&subscription, b"x", &vector(), &salt, "openssl", &tmp)
            .expect_err("a 64-octet point is not a point");
        assert!(sentence.contains("p256dh"), "{sentence}");
        subscription.p256dh = vec![0x04; 65];
        subscription.auth = vec![0; 15];
        let sentence = seal(&subscription, b"x", &vector(), &salt, "openssl", &tmp)
            .expect_err("a 15-octet auth secret is not one");
        assert!(sentence.contains("auth"), "{sentence}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fresh_salt_is_16_octets_and_they_differ() {
        let first = salt().expect("this machine has /dev/urandom");
        let second = salt().expect("this machine has /dev/urandom");
        assert_eq!(first.len(), SALT);
        assert_ne!(first, second, "two messages got one salt");
    }
}
