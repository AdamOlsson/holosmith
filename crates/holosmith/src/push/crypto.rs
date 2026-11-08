//! The four primitives a Web Push message needs, each pinned by its own published vector (HS-066, ADR-0071).
//!
//! There is no crypto dependency in this crate and no plan to add one (ADR-0001, and `check-static.sh` guards
//! `Cargo.lock`), and `openssl enc` cannot do AEAD on this host (measured: `AEAD ciphers not supported`,
//! OpenSSL 3.0.13). So the message's confidentiality is written out here: HMAC-SHA-256 (RFC 2104), HKDF
//! (RFC 5869), AES-128's encryption half (FIPS 197) and GCM's GHASH (NIST SP 800-38D).
//!
//! **Why a hand-written primitive is the smaller risk here.** Every function below is small enough to check by
//! eye, and every one is pinned by a vector published by the document that defines it: RFC 4231's two HMAC
//! cases, RFC 5869's three HKDF cases, FIPS 197's Appendix C.1, and two NIST GCM cases. The story's own golden
//! test is RFC 8291 §5's complete example, byte for byte, which is these four functions composed exactly as the
//! RFC composes them — so a mistake in any one of them fails a published-vector test by name. What bounds the
//! stake is the payload: the plaintext is `{host, session_id, kind}` and never a message's content (spec item
//! 11), so the confidentiality this protects is a session's existence, not its transcript.
//!
//! **Encryption only, wherever an inverse can be avoided.** [`seal`] is what production calls; [`open`] exists so
//! a test can prove the round trip and prove that a tampered tag is refused, and it is compiled only under
//! `cfg(test)`. There is no constant-time claim anywhere in this file: the plaintext is content-free by
//! construction and the key is derived from a subscription the browser handed us, so a timing signal would say
//! what the payload already says.
//!
//! SHA-256 is **not** written a second time: [`crate::images::sha256`] is this crate's FIPS 180-4
//! implementation, pinned by the standard's own vectors, and HMAC is built on it.

/// The block size HMAC pads every key to (RFC 2104's `B`, which for SHA-256 is 64 octets).
const BLOCK: usize = 64;

/// [`crate::images::sha256`]'s digest length, which is also the length of one HKDF block (RFC 5869's `HashLen`).
const DIGEST: usize = 32;

/// RFC 5869's own ceiling on an expansion: 255 blocks, because the counter is one octet.
pub const MAX_OUT: usize = 255 * DIGEST;

/// The Rijndael S-box (FIPS 197 Figure 7), the one table in this module that a *value* indexes.
///
/// That is what AES is, so it is not a shortcut: the substitution is defined as this table. It is acceptable
/// here for the reason the module's doc gives — the plaintext is content-free by construction (spec item 11) —
/// and because the alternative (a bitsliced or vectorised AES) is a second programme with no vector in this
/// repository to pin it.
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// The round constants (FIPS 197 §5.2), one per round after the first.
const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

/// HMAC-SHA-256 (RFC 2104) over the concatenation of `parts`.
///
/// A slice of slices rather than one buffer, because every caller here hashes two or three pieces and one of them
/// (HKDF's expansion) is a block it just computed: building the concatenation at each call site would be the same
/// allocation with the boundary spelled differently in four places.
///
/// A key longer than the 64-octet block is hashed first, which is RFC 2104's own rule and the one arm a naive
/// ipad/opad gets wrong: `hmac_sha256(&[0xff; 200], …)` would otherwise XOR 136 bytes into a 64-byte buffer.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut padded = [0_u8; BLOCK];
    if key.len() > BLOCK {
        padded[..DIGEST].copy_from_slice(&crate::images::sha256(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0x36_u8; BLOCK];
    let mut outer = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner[index] ^= padded[index];
        outer[index] ^= padded[index];
    }
    let mut message = Vec::with_capacity(BLOCK + 64);
    message.extend_from_slice(&inner);
    for part in parts {
        message.extend_from_slice(part);
    }
    let digest = crate::images::sha256(&message);
    let mut message = Vec::with_capacity(BLOCK + DIGEST);
    message.extend_from_slice(&outer);
    message.extend_from_slice(&digest);
    crate::images::sha256(&message)
}

/// HKDF-SHA-256 (RFC 5869): extract, then expand into `out`.
///
/// An **empty salt is 32 zero octets** (§2.2), not "no HMAC": RFC 5869's test case 3 is exactly that arm, and it
/// is the one every caller here would have got wrong by skipping extract for a salt it did not have.
///
/// `out` must be at most [`MAX_OUT`] octets; a longer request stops at the ceiling rather than repeating a
/// counter octet, and no caller in this crate asks for more than 32.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    const ZERO: [u8; DIGEST] = [0; DIGEST];
    let prk = if salt.is_empty() {
        hmac_sha256(&ZERO, &[ikm])
    } else {
        hmac_sha256(salt, &[ikm])
    };
    let mut previous: Vec<u8> = Vec::with_capacity(DIGEST);
    let mut filled = 0;
    let mut counter: u8 = 1;
    while filled < out.len() && counter > 0 {
        let count = [counter];
        let mut parts: Vec<&[u8]> = Vec::with_capacity(3);
        if !previous.is_empty() {
            parts.push(&previous);
        }
        parts.push(info);
        parts.push(&count);
        let block = hmac_sha256(&prk, &parts);
        let take = (out.len() - filled).min(DIGEST);
        out[filled..filled + take].copy_from_slice(&block[..take]);
        filled += take;
        previous = block.to_vec();
        counter = counter.wrapping_add(1);
    }
}

/// The round keys: the cipher key followed by ten more 16-octet words, 176 octets in all (FIPS 197 §5.2).
fn expand(key: &[u8; 16]) -> [u8; 176] {
    let mut schedule = [0_u8; 176];
    schedule[..16].copy_from_slice(key);
    for round in 1..=10 {
        let at = round * 16;
        let previous = at - 16;
        // The word that closes the previous round key, rotated a byte, substituted, and XORed with `Rcon`.
        let word = [
            SBOX[schedule[previous + 13] as usize] ^ RCON[round - 1],
            SBOX[schedule[previous + 14] as usize],
            SBOX[schedule[previous + 15] as usize],
            SBOX[schedule[previous + 12] as usize],
        ];
        for index in 0..4 {
            schedule[at + index] = schedule[previous + index] ^ word[index];
        }
        for index in 4..16 {
            schedule[at + index] = schedule[at + index - 4] ^ schedule[previous + index];
        }
    }
    schedule
}

/// `2 * a` in GF(2^8) with AES's polynomial: the doubling `MixColumns` is built from.
fn xtime(value: u8) -> u8 {
    (value << 1) ^ if value & 0x80 != 0 { 0x1b } else { 0 }
}

/// One round's `MixColumns` (FIPS 197 §5.1.3), over the four columns of a column-major state.
fn mix_columns(state: &mut [u8; 16]) {
    for column in state.as_chunks_mut::<4>().0 {
        let [a0, a1, a2, a3] = *column;
        column[0] = xtime(a0) ^ (xtime(a1) ^ a1) ^ a2 ^ a3;
        column[1] = a0 ^ xtime(a1) ^ (xtime(a2) ^ a2) ^ a3;
        column[2] = a0 ^ a1 ^ xtime(a2) ^ (xtime(a3) ^ a3);
        column[3] = (xtime(a0) ^ a0) ^ a1 ^ a2 ^ xtime(a3);
    }
}

/// AES-128's encryption half: one 16-octet block under one 16-octet key (FIPS 197 §5.1).
///
/// The state is the block in column-major order — `state[4 * column + row]` — which is the layout FIPS 197's
/// figures use and the one that makes `ShiftRows` a `(column + row) % 4` and `MixColumns` four consecutive
/// bytes.
pub fn aes128_encrypt(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let schedule = expand(key);
    let mut state = *block;
    add_round_key(&mut state, &schedule, 0);
    for round in 1..=9 {
        for byte in state.iter_mut() {
            *byte = SBOX[*byte as usize];
        }
        state = shift_rows(&state);
        mix_columns(&mut state);
        add_round_key(&mut state, &schedule, round);
    }
    for byte in state.iter_mut() {
        *byte = SBOX[*byte as usize];
    }
    state = shift_rows(&state);
    add_round_key(&mut state, &schedule, 10);
    state
}

fn add_round_key(state: &mut [u8; 16], schedule: &[u8; 176], round: usize) {
    for (byte, key) in state.iter_mut().zip(&schedule[round * 16..round * 16 + 16]) {
        *byte ^= key;
    }
}

/// Row `r` moves left by `r` columns, and nothing else moves.
fn shift_rows(state: &[u8; 16]) -> [u8; 16] {
    let mut shifted = [0_u8; 16];
    for column in 0..4 {
        for row in 0..4 {
            shifted[4 * column + row] = state[4 * ((column + row) % 4) + row];
        }
    }
    shifted
}

/// `inc32` (SP 800-38D §6.1): the rightmost four octets of the counter block, plus one, modulo 2^32.
fn inc32(block: &[u8; 16]) -> [u8; 16] {
    let mut next = *block;
    let counter = u32::from_be_bytes([next[12], next[13], next[14], next[15]]).wrapping_add(1);
    next[12..].copy_from_slice(&counter.to_be_bytes());
    next
}

/// GHASH's field multiplication (SP 800-38D Algorithm 1), bit at a time.
///
/// The slow version on purpose: it is eight lines that can be checked against §6.3 by eye, and the vectors
/// below are what prove it. Bit 0 of the polynomial representation is the most significant bit of the first
/// octet, which is why the block is read big-endian and `R` sits at the top.
fn multiply(x: u128, y: u128) -> u128 {
    /// `R = 11100001 || 0^120`, the reduction polynomial's low half (§6.3).
    const R: u128 = 0xe1 << 120;
    let mut product = 0_u128;
    let mut shifted = y;
    for bit in 0..128 {
        if (x >> (127 - bit)) & 1 == 1 {
            product ^= shifted;
        }
        shifted = if shifted & 1 == 1 {
            (shifted >> 1) ^ R
        } else {
            shifted >> 1
        };
    }
    product
}

/// GHASH over `ciphertext` with an **empty** associated-data field, which is what RFC 8291's `aes128gcm` body
/// has: a push message's only data is the encrypted record.
fn ghash(h: u128, ciphertext: &[u8]) -> u128 {
    let mut accumulator = 0_u128;
    for chunk in ciphertext.chunks(16) {
        let mut block = [0_u8; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        accumulator = multiply(accumulator ^ u128::from_be_bytes(block), h);
    }
    // The length block: `len(A) || len(C)` in bits, each a 64-bit big-endian integer. `A` is empty.
    let mut lengths = [0_u8; 16];
    lengths[8..].copy_from_slice(&((ciphertext.len() as u64).wrapping_mul(8)).to_be_bytes());
    multiply(accumulator ^ u128::from_be_bytes(lengths), h)
}

/// `data` XOR the GCM keystream beginning at `start`, counter and all.
///
/// One helper for the ciphertext and for nothing else: the tag's mask is `E(J0)` (one block, [`seal`] computes
/// it directly), while the record's keystream begins at `inc32(J0)` — that one-block offset is the whole of
/// GCM's counter convention and is the easiest thing in this file to get wrong.
fn keystream(key: &[u8; 16], start: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut counter = *start;
    for chunk in data.chunks(16) {
        let block = aes128_encrypt(key, &counter);
        out.extend(chunk.iter().zip(block).map(|(byte, key)| byte ^ key));
        counter = inc32(&counter);
    }
    out
}

/// AES-128-GCM's seal: the ciphertext followed by the 16-octet tag (SP 800-38D §7.1).
///
/// A 12-octet nonce **only**, and the type says so: a nonce of another length needs `J0 = GHASH(IV)` and pads
/// the IV to a 128-bit boundary, and RFC 8291 §4 always derives 12 octets. Refusing the other lengths in the
/// signature is the one refusal here that cannot be forgotten at a call site.
pub fn seal(key: &[u8; 16], nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
    let h = u128::from_be_bytes(aes128_encrypt(key, &[0_u8; 16]));
    let mut j0 = [0_u8; 16];
    j0[..12].copy_from_slice(nonce);
    j0[15] = 1;
    let mask = aes128_encrypt(key, &j0);
    let ciphertext = keystream(key, &inc32(&j0), plaintext);
    let tag = ghash(h, &ciphertext) ^ u128::from_be_bytes(mask);
    let mut sealed = ciphertext;
    sealed.extend_from_slice(&tag.to_be_bytes());
    sealed
}

/// [`seal`]'s inverse, and a refusal for a tag that does not match.
///
/// Compiled for tests only: production encrypts and never decrypts, because the machine is the sender and a
/// browser is the receiver. It lives here rather than in a test module so the round trip measures the shipped
/// [`seal`] rather than a copy of it.
#[cfg(test)]
pub(crate) fn open(key: &[u8; 16], nonce: &[u8; 12], sealed: &[u8]) -> Option<Vec<u8>> {
    let (ciphertext, tag) = sealed.split_at(sealed.len().checked_sub(16)?);
    let h = u128::from_be_bytes(aes128_encrypt(key, &[0_u8; 16]));
    let mut j0 = [0_u8; 16];
    j0[..12].copy_from_slice(nonce);
    j0[15] = 1;
    let mask = aes128_encrypt(key, &j0);
    let expected = ghash(h, ciphertext) ^ u128::from_be_bytes(mask);
    if expected.to_be_bytes() != tag {
        return None;
    }
    Some(keystream(key, &inc32(&j0), ciphertext))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lowercase hex, so a test can quote a vector byte for byte.
    fn hex(bytes: &[u8]) -> String {
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
        }
        text
    }

    /// Unhex a vector's own spelling, so the vectors below are the documents' strings.
    fn unhex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|byte| byte.is_ascii_hexdigit())
            .collect();
        digits
            .chunks(2)
            .map(|pair| {
                let high = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
                let low = (pair[1] as char).to_digit(16).unwrap_or(0) as u8;
                high * 16 + low
            })
            .collect()
    }

    #[test]
    fn hmac_sha256_matches_rfc_4231s_first_two_cases() {
        // RFC 4231 §4.2: a 20-octet key of 0x0b over "Hi There".
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], &[b"Hi There"])),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // §4.3: a short ASCII key, which is the arm that catches a padded buffer that is not zeroed.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", &[b"what do ya want for nothing?"])),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // The concatenation is one message: two parts hash like their join, which is the only claim `parts`
        // makes.
        assert_eq!(
            hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]),
            hmac_sha256(b"Jefe", &[b"what do ya want for nothing?"])
        );
        // §4.6: a 131-octet key of 0xaa over the published message. The arm matters because RFC 2104 §2 hashes a
        // key longer than the block first, and a 64-octet buffer without that rule would read past it — so this
        // asserts the *digest*, not merely that nothing panicked.
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hkdf_sha256_matches_rfc_5869s_three_cases() {
        // §A.1: the ordinary arm — a salt, an info and 42 octets, which is more than one block.
        let mut out = [0_u8; 42];
        hkdf_sha256(
            &unhex("000102030405060708090a0b0c"),
            &[0x0b; 22],
            &unhex("f0f1f2f3f4f5f6f7f8f9"),
            &mut out,
        );
        assert_eq!(
            hex(&out),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
        // §A.2: 80-octet inputs and an 82-octet output, so three blocks and a partial one.
        let mut out = [0_u8; 82];
        hkdf_sha256(
            &unhex(
                "606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f808182838485868788\
                 898a8b8c8d8e8f909192939495969798999a9b9c9d9e9fa0a1a2a3a4a5a6a7a8a9aaabacadaeaf",
            ),
            &unhex(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f2021222324252627\
                 28292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f",
            ),
            &unhex(
                "b0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8\
                 d9dadbdcdddedfe0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
            ),
            &mut out,
        );
        assert_eq!(
            hex(&out),
            "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c59045a99cac7827271cb41c65e590e09\
             da3275600c2f09b8367793a9aca3db71cc30c58179ec3e87c14c01d5c1f3434f1d87"
        );
        // §A.3: the arm that decides whether an empty salt means "no extract" or "32 zero octets".
        let mut out = [0_u8; 42];
        hkdf_sha256(&[], &[0x0b; 22], &[], &mut out);
        assert_eq!(
            hex(&out),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
        );
    }

    #[test]
    fn aes128_matches_fips_197s_appendix_c1() {
        // FIPS 197 §C.1's key and plaintext, and the ciphertext it gives.
        let key: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .expect("a 16-byte key");
        let plaintext: [u8; 16] = unhex("00112233445566778899aabbccddeeff")
            .try_into()
            .expect("a 16-byte block");
        assert_eq!(
            hex(&aes128_encrypt(&key, &plaintext)),
            "69c4e0d86a7b0430d8cdb78070b4c55a"
        );
    }

    #[test]
    fn gcm_matches_nist_sp_800_38ds_two_96_bit_iv_cases() {
        // Test case 1: the zero key, the zero IV, an empty plaintext — the tag alone, which is GHASH's length
        // block with no ciphertext to fold in.
        let key = [0_u8; 16];
        let nonce = [0_u8; 12];
        let sealed = seal(&key, &nonce, &[]);
        assert_eq!(hex(&sealed), "58e2fccefa7e3061367f1d57a4e7455a");

        // Test case 2: the same key and IV over one block of zeros. The ciphertext is the keystream at
        // `inc32(J0)`, and the tag masks GHASH with `E(J0)`.
        let plaintext = [0_u8; 16];
        let sealed = seal(&key, &nonce, &plaintext);
        assert_eq!(
            hex(&sealed),
            "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"
        );
    }

    #[test]
    fn seal_and_open_round_trip_and_a_tampered_tag_is_refused() {
        let key: [u8; 16] = unhex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .expect("a 16-byte key");
        let nonce: [u8; 12] = unhex("000102030405060708090a0b")
            .try_into()
            .expect("a 12-byte nonce");
        // Three plaintexts, and the third is the one that matters: 17 octets crosses a block, so a keystream
        // that forgot to advance its counter decrypts the tail into something that is not the plaintext.
        for plaintext in [
            Vec::new(),
            b"0123456789abcdef".to_vec(),
            b"0123456789abcdef!".to_vec(),
        ] {
            let sealed = seal(&key, &nonce, &plaintext);
            assert_eq!(sealed.len(), plaintext.len() + 16);
            assert_eq!(open(&key, &nonce, &sealed).as_deref(), Some(&plaintext[..]));
            // One flipped bit of the tag, and one of the ciphertext: both are refusals rather than a wrong
            // plaintext.
            let mut bad_tag = sealed.clone();
            let last = bad_tag.len() - 1;
            bad_tag[last] ^= 0x01;
            assert_eq!(open(&key, &nonce, &bad_tag), None);
            if !plaintext.is_empty() {
                let mut bad_body = sealed.clone();
                bad_body[0] ^= 0x80;
                assert_eq!(open(&key, &nonce, &bad_body), None);
            }
            // A truncated answer is not a plaintext either.
            assert_eq!(open(&key, &nonce, &sealed[..sealed.len() - 1]), None);
        }
    }

    #[test]
    fn the_inc32_counter_wraps_at_its_own_width() {
        // The one arithmetic GCM's counter has, pinned on its own so a keystream bug names itself.
        let mut block = [0_u8; 16];
        block[15] = 0xff;
        assert_eq!(inc32(&block)[15], 0x00);
        assert_eq!(inc32(&block)[12], 0x00);
        let block = [0xff_u8; 16];
        let next = inc32(&block);
        assert_eq!(&next[12..], &[0, 0, 0, 0]);
        assert_eq!(&next[..12], &[0xff; 12], "inc32 touched more than 32 bits");
    }
}
