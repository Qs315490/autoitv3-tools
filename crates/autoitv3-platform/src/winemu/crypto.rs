//! The primitives behind the emulated CryptoAPI.
//!
//! The reference sample decrypts its payload with `advapi32`'s CryptoAPI:
//! `CryptAcquireContext` → `CryptCreateHash` (MD5 or SHA-1) → `CryptHashData` →
//! `CryptDeriveKey` (RC4/AES) → `CryptDecrypt`. Off Windows none of that
//! exists, so the algorithms are implemented here — deliberately the small,
//! self-contained ones:
//!
//! * hashes: MD5, SHA-1, SHA-256 (the `CALG_*` ids the sample uses)
//! * ciphers: RC4 (`CALG_RC4`), which is what the string table's key derivation
//!   produces
//!
//! There is no unverified fallback: an algorithm that is not implemented makes
//! the `Crypt*` call fail, so the script takes its own error path instead of
//! silently working on garbage.
//!
//! The primitives come from RustCrypto (`md-5`/`sha1`/`sha2`/`aes`/`rc4`) —
//! this module used to carry hand-written MD5, SHA-1, SHA-256, AES and RC4,
//! which was a lot of security-relevant code to keep correct. Only the
//! CryptoAPI-specific glue lives here now: which `CALG_*` id is which
//! algorithm, and the `CryptDeriveKey` rule below. The published test vectors
//! in `tests/unit/winemu_crypto.rs` pin the wrappers just as tightly as they
//! pinned the originals.

use sha2::Digest;

/// Which hash a `CryptCreateHash` handle is accumulating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlg {
    Md5,
    Sha1,
    Sha256,
}

impl HashAlg {
    /// Map a `CALG_*` id onto an algorithm.
    ///
    /// * `CALG_MD5` = `0x8003`
    /// * `CALG_SHA1` = `0x8004`
    /// * `CALG_SHA_256` = `0x800c`
    pub fn from_algid(algid: u32) -> Option<Self> {
        match algid & 0xffff {
            0x8003 => Some(HashAlg::Md5),
            0x8004 => Some(HashAlg::Sha1),
            0x800c => Some(HashAlg::Sha256),
            _ => None,
        }
    }

    /// The digest length in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            HashAlg::Md5 => 16,
            HashAlg::Sha1 => 20,
            HashAlg::Sha256 => 32,
        }
    }

    /// Whether this hash belongs to the SHA-2 family. `CryptDeriveKey` calls
    /// out the distinction when deriving 3DES/AES keys.
    pub fn is_sha2(self) -> bool {
        matches!(self, HashAlg::Sha256)
    }

    /// Digest `data`.
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Md5 => md5(data).to_vec(),
            HashAlg::Sha1 => sha1(data).to_vec(),
            HashAlg::Sha256 => sha256(data).to_vec(),
        }
    }
}

/// A symmetric cipher a derived key can be used with.
///
/// The sample derives all three AES widths plus `CALG_RC4` keys and decrypts in
/// CBC mode with the IV `CryptDeriveKey` leaves on the key (all zeroes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherAlg {
    /// `CALG_RC4` — a stream cipher, so decryption is `data XOR keystream`.
    Rc4,
    /// `CALG_AES_128` (`0x660e`).
    Aes128,
    /// `CALG_AES_192` (`0x660f`).
    Aes192,
    /// `CALG_AES_256` (`0x6610`).
    Aes256,
}

impl CipherAlg {
    /// Map a `CALG_*` id onto a cipher. 3DES and the other algorithms are not
    /// implemented, and saying so is better than decrypting to rubbish.
    ///
    /// The AES ids run `0x660e`, `0x660f`, `0x6610` for 128, 192 and 256 bits,
    /// following `ALG_SID_AES_128` … `ALG_SID_AES_256`.
    pub fn from_algid(algid: u32) -> Option<Self> {
        match algid & 0xffff {
            0x6801 => Some(CipherAlg::Rc4),
            0x660e => Some(CipherAlg::Aes128),
            0x660f => Some(CipherAlg::Aes192),
            0x6610 => Some(CipherAlg::Aes256),
            _ => None,
        }
    }

    /// The key size in bytes.
    pub fn key_len(self) -> usize {
        match self {
            CipherAlg::Rc4 => 16,
            CipherAlg::Aes128 => 16,
            CipherAlg::Aes192 => 24,
            CipherAlg::Aes256 => 32,
        }
    }

    /// The block size in bytes (`1` for a stream cipher).
    pub fn block_len(self) -> usize {
        match self {
            CipherAlg::Rc4 => 1,
            _ => 16,
        }
    }

    /// Decrypt `data` (RC4 is symmetric; AES is CBC-decrypted with `iv`).
    pub fn apply(self, key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
        match self {
            CipherAlg::Rc4 => rc4(key, data),
            _ => aes_cbc_decrypt(key, iv, data),
        }
    }

    /// Whether this is a block cipher — the distinction `CryptDeriveKey` draws
    /// when it decides how a digest becomes key material.
    pub fn is_block(self) -> bool {
        !matches!(self, CipherAlg::Rc4)
    }

    /// Turn a `CryptCreateHash`/`CryptHashData` digest into this cipher's key
    /// and IV, following the rule documented for `CryptDeriveKey`:
    ///
    /// * normally the key is the first `key_len` bytes of the digest;
    /// * but "if the hash is not a member of the SHA-2 family and the required
    ///   key is for either 3DES or AES", the digest is instead mixed into 64
    ///   bytes of `0x36` and 64 bytes of `0x5c`, each buffer is hashed with the
    ///   same algorithm, and the two digests are concatenated. That HMAC-shaped
    ///   expansion is what lets a 16-byte MD5 password hash fill a 256-bit AES
    ///   key.
    ///
    /// Block ciphers keep CryptoAPI's default all-zero CBC IV; a stream cipher
    /// has no IV at all.
    pub fn derive_key(self, hash: HashAlg, digest: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let material = if self.is_block() && !hash.is_sha2() {
            let mut inner = [0x36u8; 64];
            let mut outer = [0x5cu8; 64];
            for (i, byte) in digest.iter().enumerate().take(64) {
                inner[i] ^= byte;
                outer[i] ^= byte;
            }
            let mut expanded = hash.digest(&inner);
            expanded.extend_from_slice(&hash.digest(&outer));
            expanded
        } else {
            digest.to_vec()
        };
        let key = material.iter().copied().take(self.key_len()).collect();
        let iv = if self.is_block() {
            vec![0u8; self.block_len()]
        } else {
            Vec::new()
        };
        (key, iv)
    }
}

// ---------------------------------------------------------------------------
// Primitives (RustCrypto)
// ---------------------------------------------------------------------------

/// CBC-decrypt `data`; a trailing partial block is left as it is.
///
/// CryptoAPI's CBC has no padding, so this cannot use `cbc`'s padded helpers:
/// a short final block is passed through untouched, which is what the
/// hand-written loop this replaced did.
pub fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockDecrypt, KeyInit};

    let mut prev = [0u8; 16];
    for (i, b) in iv.iter().take(16).enumerate() {
        prev[i] = *b;
    }
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(16) {
        if block.len() < 16 {
            out.extend_from_slice(block);
            break;
        }
        let mut input = [0u8; 16];
        input.copy_from_slice(block);
        let mut plain = input;
        // Callers derive the key through `CipherAlg`, so the length is one of
        // these three; anything else leaves the rest undecrypted rather than
        // panicking inside a `Crypt*` call.
        match key.len() {
            16 => aes::Aes128::new_from_slice(key)
                .expect("16-byte key")
                .decrypt_block((&mut plain).into()),
            24 => aes::Aes192::new_from_slice(key)
                .expect("24-byte key")
                .decrypt_block((&mut plain).into()),
            32 => aes::Aes256::new_from_slice(key)
                .expect("32-byte key")
                .decrypt_block((&mut plain).into()),
            _ => break,
        }
        for i in 0..16 {
            out.push(plain[i] ^ prev[i]);
        }
        prev = input;
    }
    out
}

/// MD5 of `input`.
pub fn md5(input: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&md5::Md5::digest(input));
    out
}

/// SHA-1 of `input`.
pub fn sha1(input: &[u8]) -> [u8; 20] {
    let mut out = [0u8; 20];
    out.copy_from_slice(&sha1::Sha1::digest(input));
    out
}

/// SHA-256 of `input`.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&sha2::Sha256::digest(input));
    out
}

/// RC4 keystream XOR — `key` is the RC4 key, `data` the bytes to transform.
///
/// RC4 is symmetric, so the same call both encrypts and decrypts. An empty key
/// leaves the data untouched (and the caller should have rejected it).
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    use rc4::{KeyInit, StreamCipher};

    if key.is_empty() {
        return data.to_vec();
    }
    // The crate rejects keys outside 1..=256 bytes; the caller checks the
    // derived key, so falling back to a pass-through only happens for input
    // that was already invalid.
    let Ok(mut cipher) = rc4::Rc4::new_from_slice(key) else {
        return data.to_vec();
    };
    let mut out = data.to_vec();
    cipher.apply_keystream(&mut out);
    out
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/winemu_crypto.rs"]
mod tests;
