//! The machine's Web Push key pair (spec item 10 step 2's "Generate the VAPID keys").
//!
//! One file, `<data root>/secrets/vapid.toml`, mode 0600: a P-256 key pair as two base64url strings — the 65-byte
//! uncompressed public point (leading `0x04`) and the 32-byte private scalar. It is a **separate file** from
//! `keys.toml` on purpose: that one is the provider-key store `key set` and `key list` own, and a VAPID pair is
//! not a provider key.
//!
//! `server deploy` is the file's only writer, and it writes it **once**. Three facts make "never regenerated"
//! true rather than intended, and each has a test: the deploy's read answers "generate" only when the path is
//! absent; a present file that does not parse is a failed step whose sentence says so (regenerating would
//! invalidate every push subscription made against the old pair); and a second deploy leaves the bytes identical.
//!
//! Generation is one child — `openssl ecparam -name prime256v1 -genkey -noout -outform DER` — so this module
//! owns the SEC1 walk that turns its stdout into the two strings ([`from_der`]), and the base64url alphabet
//! itself: the crate has no encoder dependency and no `unsafe` to reach one with.
//!
//! **The file holds no configuration.** A JWT's `sub` claim is not key material, so it is not stored here; the
//! consumer that signs with this pair decides it, and it must read this file as it is.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The one file, under the data root's `secrets` directory.
pub const VAPID_FILE: &str = "vapid.toml";

/// The file's first line, so a reader who opens it by hand knows why it is never rewritten.
const HEADER: &str =
    "# The machine's Web Push key pair. `server deploy` writes this once and never rewrites it.";

/// The pair as the file carries it: base64url, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The 65-byte uncompressed point, base64url — 87 characters.
    pub public_key: String,
    /// The 32-byte scalar, base64url — 43 characters.
    pub private_key: String,
}

/// `<data root>/secrets/vapid.toml` — the one path, computed in one place so a reader and a writer cannot
/// disagree about where the pair lives.
pub fn store(data_root: &Path) -> PathBuf {
    data_root.join(crate::key::SECRETS_DIR).join(VAPID_FILE)
}

/// The pair as the file carries it: the header, then the two assignments.
pub fn render(pair: &Pair) -> String {
    format!(
        "{HEADER}\npublic_key = \"{}\"\nprivate_key = \"{}\"\n",
        pair.public_key, pair.private_key
    )
}

/// Read a pair out of [`render`]'s text, or `None` when the text is not one.
///
/// A tiny reader rather than a TOML parser: the file is ours and its shape is two strings. It is strict about
/// that shape — **any** line that is not blank, a `#` comment or one of the two assignments is a refusal, so a
/// file somebody edited into something else is not silently read as a pair — and strict about the values, which
/// must be a 65-byte point starting `0x04` and a 32-byte scalar.
pub fn parse(text: &str) -> Option<Pair> {
    let mut public = None;
    let mut private = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        let slot = match key.trim() {
            "public_key" if public.is_none() => &mut public,
            "private_key" if private.is_none() => &mut private,
            // A repeated or unknown key is not a pair either: a file with two `public_key` lines does not say
            // which one is the machine's.
            _ => return None,
        };
        *slot = Some(value.to_string());
    }
    let pair = Pair {
        public_key: public?,
        private_key: private?,
    };
    is_well_formed(&pair).then_some(pair)
}

/// The base64url encoding, unpadded — RFC 4648's alphabet with `-_` instead of `+/`, which is what a Web Push
/// application server key is spelled in.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0_u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let bits =
            (u32::from(buffer[0]) << 16) | (u32::from(buffer[1]) << 8) | u32::from(buffer[2]);
        for index in 0..=chunk.len() {
            let shift = 18 - index * 6;
            text.push(char::from(ALPHABET[((bits >> shift) & 0x3f) as usize]));
        }
    }
    text
}

/// [`base64url`]'s inverse, `None` for anything that is not unpadded base64url — `=` padding included, since the
/// file's own shape has none.
pub fn unbase64url(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let mut bits: u32 = 0;
    let mut at: u32 = 0;
    for character in text.chars() {
        let value = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => character as u32 - 'a' as u32 + 26,
            '0'..='9' => character as u32 - '0' as u32 + 52,
            '-' => 62,
            '_' => 63,
            _ => return None,
        };
        at = (at << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(((at >> bits) & 0xff) as u8);
            at &= (1 << bits) - 1;
        }
    }
    // A well-formed encoding has zero bits left over; a stray character that only fits in the last five places is
    // a different string, and reading it as this one would make two encodings of one key.
    (at == 0).then_some(bytes)
}

/// The machine's **application server key** — the pair's public half, which is public by design: it is the `k=` of
/// every JWT this machine signs and the value a browser hands to `pushManager.subscribe` (ADR-0072).
///
/// `None` when this machine has no pair, and also when the file is there and is not one: a browser must not be
/// handed a key that is not a key, and the two states are the same answer to the only question the caller asks.
/// `Pusher::pair` is the reader that tells them apart, because its caller has a log line to spend on the
/// difference. Read at the moment of asking rather than cached, so a machine that ran `server deploy` since the
/// last request answers with the new pair.
pub fn public_key(data_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(store(data_root)).ok()?;
    parse(&text).map(|pair| pair.public_key)
}

/// Whether `path` holds a pair: `Ok(false)` when it is not there at all, `Err` when it is there and is not one.
///
/// The three answers are the deploy's whole "never regenerate" rule: absent is the only state that earns a
/// generation, and a present file it cannot read is a failure rather than a file to replace.
pub fn is_pair(path: &Path) -> Result<bool, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "{} could not be read ({error}): it holds the machine's Web Push key pair",
                path.display()
            ));
        }
    };
    if parse(&text).is_some() {
        return Ok(true);
    }
    Err(format!(
        "{} is not a VAPID key pair: it must hold a 65-byte uncompressed P-256 public point and its 32-byte \
         private scalar, each as an unpadded base64url `public_key` and `private_key`, which is what `server \
         deploy` writes",
        path.display()
    ))
}

/// Whether two strings are the two halves of a P-256 pair as far as their encodings prove it.
fn is_well_formed(pair: &Pair) -> bool {
    unbase64url(&pair.public_key).is_some_and(|point| point.len() == 65 && point[0] == 0x04)
        && unbase64url(&pair.private_key).is_some_and(|scalar| scalar.len() == 32)
}

/// The pair inside the SEC1 DER that `openssl ecparam -name prime256v1 -genkey -noout -outform DER` writes.
///
/// The walk is exact because the producer is one known command: a SEQUENCE holding version 1, the 32-byte private
/// scalar as an OCTET STRING, the named curve as context `[0]` (skipped — the command names it, and a point of
/// another curve would fail the 65-byte length), and the public key as context `[1]` holding a BIT STRING whose
/// zero unused-bits byte `0x00` precedes the 65-byte point. Anything else is refused with a sentence naming what
/// was expected, because a pair half-understood is a pair whose signatures would be wrong in a way only a phone
/// would notice.
pub fn from_der(der: &[u8]) -> Result<Pair, String> {
    let mut outer = Reader { bytes: der, at: 0 };
    let sequence = outer.element(0x30, "the key as a SEQUENCE (0x30)")?;
    if outer.at != der.len() {
        return Err(wanted("nothing after the SEQUENCE"));
    }
    let mut key = Reader {
        bytes: sequence,
        at: 0,
    };
    let version = key.element(0x02, "the key's version as an INTEGER (0x02)")?;
    if version != [0x01] {
        return Err(wanted("version 1, the only SEC1 version"));
    }
    let scalar = key.element(0x04, "the private scalar as an OCTET STRING (0x04)")?;
    if scalar.len() != 32 {
        return Err(wanted(&format!(
            "a 32-byte private scalar, where this one is {} bytes",
            scalar.len()
        )));
    }
    key.element(0xa0, "the curve as context [0] (0xa0)")?;
    let wrapper = key.element(0xa1, "the public key as context [1] (0xa1)")?;
    if key.at != sequence.len() {
        return Err(wanted("nothing after the public key"));
    }
    let mut public = Reader {
        bytes: wrapper,
        at: 0,
    };
    let bits = public.element(0x03, "the public key as a BIT STRING (0x03)")?;
    let (unused, point) = bits
        .split_first()
        .ok_or_else(|| wanted("the BIT STRING's unused-bits byte"))?;
    if *unused != 0 {
        return Err(wanted(
            "a BIT STRING with no unused bits, which is a whole octet",
        ));
    }
    if point.len() != 65 || point[0] != 0x04 {
        return Err(wanted(
            "a 65-byte uncompressed point starting 0x04 after the unused-bits byte",
        ));
    }
    Ok(Pair {
        public_key: base64url(point),
        private_key: base64url(scalar),
    })
}

/// The one sentence a refusal about the DER uses, so every arm says which producer was expected.
fn wanted(what: &str) -> String {
    format!(
        "`openssl ecparam -name prime256v1 -genkey -noout -outform DER` writes a SEC1 EC private key, and this is \
         not one: {what} was expected"
    )
}

/// A cursor over DER's tag-length-value shape.
///
/// Only the short form of the length is read: the two commands in this module's sentences produce 121 bytes, and a
/// long-form length means the bytes are something else — refusing it is honest where decoding it would be a guess.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// One element with `tag`, answering its content.
    fn element(&mut self, tag: u8, what: &str) -> Result<&'a [u8], String> {
        let Some(&found) = self.bytes.get(self.at) else {
            return Err(wanted(&format!("{what} where the value ended")));
        };
        if found != tag {
            return Err(wanted(&format!(
                "{what}, not the 0x{found:02x} that is there"
            )));
        }
        self.at += 1;
        let Some(&first) = self.bytes.get(self.at) else {
            return Err(wanted(&format!("{what}, and then its length")));
        };
        self.at += 1;
        if first >= 0x80 {
            return Err(wanted(&format!(
                "{what} with a short-form length, where a long-form one is here"
            )));
        }
        let length = usize::from(first);
        let end = self.at + length;
        if end > self.bytes.len() {
            return Err(wanted(&format!(
                "{what} carrying {length} bytes, where the value ends early"
            )));
        }
        let content = &self.bytes[self.at..end];
        self.at = end;
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The private scalar the hand-built DER below carries.
    const PRIVATE: [u8; 32] = [0x11; 32];

    /// The public point: `0x04` and then the bytes 1 through 64, so the encoding is checkable by eye.
    fn point() -> [u8; 65] {
        let mut point = [0u8; 65];
        point[0] = 0x04;
        for (index, byte) in point[1..].iter_mut().enumerate() {
            *byte = (index + 1) as u8;
        }
        point
    }

    /// The DER `openssl ecparam -name prime256v1 -genkey -noout -outform DER` writes, built by hand from the shape
    /// the plan measured: `30 77`, version 1, the scalar, `prime256v1`'s OID as context `[0]`, and the point as
    /// context `[1]`'s BIT STRING with a zero unused-bits byte.
    fn der(private: &[u8; 32], point: &[u8; 65]) -> Vec<u8> {
        let mut der = vec![0x30, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];
        der.extend_from_slice(private);
        der.extend_from_slice(&[
            0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
        ]);
        der.extend_from_slice(&[0xa1, 0x44, 0x03, 0x42, 0x00]);
        der.extend_from_slice(point);
        der
    }

    /// The base64url of [`PRIVATE`] and of [`point`], written out by hand: 43 and 87 characters, which is what a
    /// P-256 pair's two fields always are.
    const PRIVATE_BASE64URL: &str = "ERERERERERERERERERERERERERERERERERERERERERE";
    const PUBLIC_BASE64URL: &str =
        "BAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0-P0A";

    #[test]
    fn a_hand_built_der_walks_to_the_pair_it_carries() {
        let pair = from_der(&der(&PRIVATE, &point())).expect("the hand-built DER is a SEC1 key");
        assert_eq!(pair.private_key, PRIVATE_BASE64URL);
        assert_eq!(pair.public_key, PUBLIC_BASE64URL);
        // The two lengths the file's consumers rely on: a 65-byte point is 87 characters and a 32-byte scalar 43.
        assert_eq!(pair.public_key.len(), 87);
        assert_eq!(pair.private_key.len(), 43);
        assert!(is_well_formed(&pair));
    }

    #[test]
    fn base64url_is_rfc_4648s_vectors_and_round_trips() {
        // RFC 4648 §10's vectors, in its URL-safe alphabet and without the padding it shows.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64url(plain.as_bytes()), encoded);
            assert_eq!(unbase64url(encoded), Some(plain.as_bytes().to_vec()));
        }
        // The three lengths a pair's own two fields and a test's odd byte need.
        let bytes: Vec<u8> = (0..=64).collect();
        for length in [1, 32, 65] {
            let slice = &bytes[..length];
            let text = base64url(slice);
            assert_eq!(unbase64url(&text).as_deref(), Some(slice));
        }
        // Strictness: `=` padding is not this alphabet, and neither is a character outside it.
        assert_eq!(unbase64url("Zg=="), None);
        assert_eq!(unbase64url("Zg+"), None);
        assert_eq!(unbase64url("Zg/"), None);
        assert_eq!(unbase64url("Z"), None);
    }

    #[test]
    fn render_and_parse_are_one_shape() {
        let pair = Pair {
            public_key: PUBLIC_BASE64URL.to_string(),
            private_key: PRIVATE_BASE64URL.to_string(),
        };
        let text = render(&pair);
        assert!(text.starts_with('#'), "the file has no header: {text}");
        assert_eq!(parse(&text), Some(pair.clone()));
        // Whitespace around `=` is tolerated, because a file an operator opened in an editor is still the file.
        let spaced = format!(
            "{}\npublic_key  =  \"{}\"\nprivate_key =\t\"{}\"\n",
            HEADER, pair.public_key, pair.private_key
        );
        assert_eq!(parse(&spaced), Some(pair));
    }

    #[test]
    fn a_file_that_is_not_a_pair_is_refused() {
        // A rogue line: the file is ours and its shape is two strings, so a third thing in it is not a pair.
        let rogue = format!(
            "{HEADER}\npublic_key = \"{PUBLIC_BASE64URL}\"\nprivate_key = \"{PRIVATE_BASE64URL}\"\nsubject = \"mailto:x\"\n"
        );
        assert_eq!(parse(&rogue), None);
        // Only one of the two.
        let half = format!("public_key = \"{PUBLIC_BASE64URL}\"\n");
        assert_eq!(parse(&half), None);
        // A pair whose fields are base64url but the wrong bytes: a 32-byte "point" is not one.
        let short = format!(
            "public_key = \"{PRIVATE_BASE64URL}\"\nprivate_key = \"{PRIVATE_BASE64URL}\"\n"
        );
        assert_eq!(parse(&short), None);
        // And a duplicate key does not say which line is the machine's.
        let twice = format!(
            "public_key = \"{PUBLIC_BASE64URL}\"\npublic_key = \"{PUBLIC_BASE64URL}\"\nprivate_key = \"{PRIVATE_BASE64URL}\"\n"
        );
        assert_eq!(parse(&twice), None);
    }

    #[test]
    fn der_that_is_not_the_shape_is_three_sentences() {
        // Too short: the walk ends with nothing to read.
        let short = der(&PRIVATE, &point())[..40].to_vec();
        let sentence = from_der(&short).expect_err("a truncated key is not a key");
        assert!(
            sentence.contains("SEC1 EC private key"),
            "the sentence does not say what was expected: {sentence}"
        );
        // The right length and shape, but the point does not start with `0x04`: not an uncompressed point.
        let mut compressed = point();
        compressed[0] = 0x03;
        let sentence = from_der(&der(&PRIVATE, &compressed)).expect_err("a point is not 0x04");
        assert!(
            sentence.contains("0x04"),
            "the sentence does not name the byte that is wrong: {sentence}"
        );
        // A long-form length is refused rather than decoded: it means the bytes are another structure.
        let mut long_form = der(&PRIVATE, &point());
        long_form[1] = 0x81;
        let sentence = from_der(&long_form).expect_err("a long-form length is not this key");
        assert!(
            sentence.contains("short-form"),
            "the sentence does not say what was refused: {sentence}"
        );
    }

    #[test]
    fn the_public_key_is_the_pairs_public_half_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("holosmith-vapid-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("secrets")).expect("the secrets directory is creatable");
        let path = store(&dir);
        // Absent, and a file that is not a pair: one answer, because a browser may be handed neither.
        assert_eq!(public_key(&dir), None);
        std::fs::write(&path, "public_key = \"not a point\"\n").expect("the file is writable");
        assert_eq!(public_key(&dir), None);

        let pair = Pair {
            public_key: PUBLIC_BASE64URL.to_string(),
            private_key: PRIVATE_BASE64URL.to_string(),
        };
        std::fs::write(&path, render(&pair)).expect("the file is writable");
        assert_eq!(public_key(&dir).as_deref(), Some(PUBLIC_BASE64URL));
        // The private half is never the answer: this is what a browser is given.
        assert_ne!(public_key(&dir).as_deref(), Some(PRIVATE_BASE64URL));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_pair_separates_absent_from_wrong() {
        let dir = std::env::temp_dir().join(format!("holosmith-vapid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is creatable");
        let path = store(&dir);
        assert_eq!(path, dir.join("secrets/vapid.toml"));

        std::fs::create_dir_all(dir.join("secrets")).expect("the secrets directory is creatable");
        assert_eq!(is_pair(&path), Ok(false), "an absent file is not an error");
        std::fs::write(&path, "garbage\n").expect("the file is writable");
        let sentence = is_pair(&path).expect_err("garbage is not a pair");
        assert!(
            sentence.contains("not a VAPID key pair"),
            "the sentence does not say what the file is not: {sentence}"
        );
        let pair = Pair {
            public_key: PUBLIC_BASE64URL.to_string(),
            private_key: PRIVATE_BASE64URL.to_string(),
        };
        std::fs::write(&path, render(&pair)).expect("the file is writable");
        assert_eq!(is_pair(&path), Ok(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
