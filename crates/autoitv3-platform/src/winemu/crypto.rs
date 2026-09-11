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
//! The hash functions are the textbook constructions and are checked against
//! the published test vectors in the module tests.

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
// AES
// ---------------------------------------------------------------------------

/// `(sbox, inv_sbox)` computed from the field arithmetic rather than
/// transcribed, so a typo cannot silently corrupt every decryption.
/// The S-box pair, computed once.
///
/// Building them costs a full field inversion per entry, and `aes_decrypt_block`
/// is called once per 16-byte block, so recomputing them per block dominated
/// every decryption of a real payload.
fn aes_tables() -> &'static ([u8; 256], [u8; 256]) {
    static TABLES: std::sync::OnceLock<([u8; 256], [u8; 256])> = std::sync::OnceLock::new();
    TABLES.get_or_init(compute_aes_tables)
}

fn compute_aes_tables() -> ([u8; 256], [u8; 256]) {
    fn mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        for _ in 0..8 {
            if b & 1 != 0 {
                p ^= a;
            }
            let hi = a & 0x80;
            a <<= 1;
            if hi != 0 {
                a ^= 0x1b;
            }
            b >>= 1;
        }
        p
    }
    let mut sbox = [0u8; 256];
    for i in 0..256u32 {
        let x = i as u8;
        // 0 has no inverse; the affine transform of 0 is what makes sbox[0] = 0x63.
        let inverse = if i == 0 {
            0
        } else {
            (1..=255u8).find(|y| mul(x, *y) == 1).unwrap_or(0)
        };
        sbox[i as usize] = inverse
            ^ inverse.rotate_left(1)
            ^ inverse.rotate_left(2)
            ^ inverse.rotate_left(3)
            ^ inverse.rotate_left(4)
            ^ 0x63;
    }
    let mut inv = [0u8; 256];
    for (i, s) in sbox.iter().enumerate() {
        inv[*s as usize] = i as u8;
    }
    (sbox, inv)
}

/// AES key schedule: `4 * (rounds + 1)` words.
fn aes_expand_key(key: &[u8]) -> Vec<[u8; 4]> {
    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];
    let (sbox, _) = aes_tables();
    let nk = key.len() / 4;
    let nr = nk + 6;
    let mut w: Vec<[u8; 4]> = key.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    for i in nk..4 * (nr + 1) {
        let mut temp = w[i - 1];
        if i % nk == 0 {
            temp = [temp[1], temp[2], temp[3], temp[0]];
            for b in temp.iter_mut() {
                *b = sbox[*b as usize];
            }
            temp[0] ^= RCON[i / nk - 1];
        } else if nk > 6 && i % nk == 4 {
            for b in temp.iter_mut() {
                *b = sbox[*b as usize];
            }
        }
        let prev = w[i - nk];
        w.push([prev[0] ^ temp[0], prev[1] ^ temp[1], prev[2] ^ temp[2], prev[3] ^ temp[3]]);
    }
    w
}

fn add_round_key(state: &mut [u8; 16], w: &[[u8; 4]], round: usize) {
    for c in 0..4 {
        for r in 0..4 {
            state[c * 4 + r] ^= w[round * 4 + c][r];
        }
    }
}

fn inv_shift_rows(s: &mut [u8; 16]) {
    let copy = *s;
    for r in 0..4 {
        for c in 0..4 {
            s[c * 4 + r] = copy[((c + 4 - r) % 4) * 4 + r];
        }
    }
}

fn inv_mix_columns(s: &mut [u8; 16]) {
    fn mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        for _ in 0..8 {
            if b & 1 != 0 {
                p ^= a;
            }
            let hi = a & 0x80;
            a <<= 1;
            if hi != 0 {
                a ^= 0x1b;
            }
            b >>= 1;
        }
        p
    }
    for c in 0..4 {
        let col = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
        s[c * 4] = mul(col[0], 14) ^ mul(col[1], 11) ^ mul(col[2], 13) ^ mul(col[3], 9);
        s[c * 4 + 1] = mul(col[0], 9) ^ mul(col[1], 14) ^ mul(col[2], 11) ^ mul(col[3], 13);
        s[c * 4 + 2] = mul(col[0], 13) ^ mul(col[1], 9) ^ mul(col[2], 14) ^ mul(col[3], 11);
        s[c * 4 + 3] = mul(col[0], 11) ^ mul(col[1], 13) ^ mul(col[2], 9) ^ mul(col[3], 14);
    }
}

/// Decrypt one 16-byte block with the inverse cipher.
///
/// The caller passes the expanded key and the S-box, so a multi-block
/// decryption does not rebuild them for every block.
fn aes_decrypt_block_with(w: &[[u8; 4]], inv_sbox: &[u8; 256], block: &[u8; 16]) -> [u8; 16] {
    let nr = w.len() / 4 - 1;
    let mut s = *block;
    add_round_key(&mut s, &w, nr);
    for round in (1..nr).rev() {
        inv_shift_rows(&mut s);
        for b in s.iter_mut() {
            *b = inv_sbox[*b as usize];
        }
        add_round_key(&mut s, &w, round);
        inv_mix_columns(&mut s);
    }
    inv_shift_rows(&mut s);
    for b in s.iter_mut() {
        *b = inv_sbox[*b as usize];
    }
    add_round_key(&mut s, w, 0);
    s
}


/// CBC-decrypt `data`; a trailing partial block is left as it is.
pub fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let (_, inv_sbox) = aes_tables();
    let w = aes_expand_key(key);
    let mut out = Vec::with_capacity(data.len());
    let mut prev: [u8; 16] = [0; 16];
    for (i, b) in iv.iter().take(16).enumerate() {
        prev[i] = *b;
    }
    for block in data.chunks(16) {
        if block.len() < 16 {
            out.extend_from_slice(block);
            break;
        }
        let mut input = [0u8; 16];
        input.copy_from_slice(block);
        let plain = aes_decrypt_block_with(&w, inv_sbox, &input);
        for i in 0..16 {
            out.push(plain[i] ^ prev[i]);
        }
        prev = input;
    }
    out
}

// ---------------------------------------------------------------------------
// MD5
// ---------------------------------------------------------------------------

/// Per-round left rotations.
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// MD5 of `input`.
pub fn md5(input: &[u8]) -> [u8; 16] {
    // K[i] = floor(2^32 * abs(sin(i + 1))), computed rather than transcribed.
    let k = |i: usize| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32;

    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let x = a
                .wrapping_add(f)
                .wrapping_add(k(i))
                .wrapping_add(m[g]);
            b = b.wrapping_add(x.rotate_left(MD5_S[i]));
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

// ---------------------------------------------------------------------------
// SHA-1
// ---------------------------------------------------------------------------

/// SHA-1 of `input`.
pub fn sha1(input: &[u8]) -> [u8; 20] {
    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());

    let (mut h0, mut h1, mut h2, mut h3, mut h4) =
        (0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0);
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5a82_7999u32),
                1 => (b ^ c ^ d, 0x6ed9_eba1),
                2 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, h) in [h0, h1, h2, h3, h4].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&h.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 of `input`.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (i, x) in v.iter().enumerate() {
            h[i] = h[i].wrapping_add(*x);
        }
    }

    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// RC4
// ---------------------------------------------------------------------------

/// RC4 keystream XOR — `key` is the RC4 key, `data` the bytes to transform.
///
/// RC4 is symmetric, so the same call both encrypts and decrypts. An empty key
/// leaves the data untouched (and the caller should have rejected it).
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s: [u8; 256] = [0; 256];
    for (i, x) in s.iter_mut().enumerate() {
        *x = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    let mut out = Vec::with_capacity(data.len());
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[s[i as usize].wrapping_add(s[j as usize]) as usize];
        out.push(byte ^ k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn md5_matches_the_published_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn sha1_matches_the_published_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rc4_matches_the_published_vector() {
        // The classic "Key" / "Plaintext" test vector.
        let out = rc4(b"Key", b"Plaintext");
        assert_eq!(hex(&out), "bbf316e8d940af0ad3");
        // Symmetric: running it again gives the plaintext back.
        assert_eq!(rc4(b"Key", &out), b"Plaintext".to_vec());
    }

    #[test]
    fn aes_cbc_matches_the_nist_vector() {
        // NIST SP 800-38A, F.2.1 (CBC-AES128.Decrypt), first block.
        let key: Vec<u8> = vec![
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv: Vec<u8> = (0x00..=0x0fu8).collect();
        let cipher: Vec<u8> = vec![
            0x76, 0x49, 0xab, 0xac, 0x81, 0x19, 0xb2, 0x46, 0xce, 0xe9, 0x8e, 0x9b, 0x12, 0xe9,
            0x19, 0x7d,
        ];
        let plain: Vec<u8> = vec![
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        assert_eq!(aes_cbc_decrypt(&key, &iv, &cipher), plain);
    }

    #[test]
    fn algorithm_ids_map_to_algorithms() {
        assert_eq!(HashAlg::from_algid(0x8003), Some(HashAlg::Md5));
        assert_eq!(HashAlg::from_algid(0x8004), Some(HashAlg::Sha1));
        assert_eq!(HashAlg::from_algid(0x800c), Some(HashAlg::Sha256));
        assert_eq!(HashAlg::from_algid(0x660e), None);
        assert_eq!(CipherAlg::from_algid(0x6801), Some(CipherAlg::Rc4));
        // The AES ids count up with the key size.
        assert_eq!(CipherAlg::from_algid(0x660e), Some(CipherAlg::Aes128));
        assert_eq!(CipherAlg::from_algid(0x660f), Some(CipherAlg::Aes192));
        assert_eq!(CipherAlg::from_algid(0x6610), Some(CipherAlg::Aes256));
        assert_eq!(CipherAlg::Aes256.key_len(), 32);
        // 3DES is deliberately absent: better a visible boundary than rubbish.
        assert_eq!(CipherAlg::from_algid(0x6603), None);
    }

    #[test]
    fn an_aes_key_wider_than_the_hash_uses_the_documented_expansion() {
        // `CryptDeriveKey` fills a 256-bit AES key from a 128-bit MD5 digest by
        // mixing the digest into 64 bytes of 0x36 and 64 bytes of 0x5c, hashing
        // each, and concatenating — the rule MSDN gives for a non-SHA-2 hash
        // feeding AES. Without it a 16-byte digest could never fill the key.
        let digest = md5(b"a password whose digest is shorter than the key");
        assert_eq!(
            digest,
            [
                0xb3, 0xbf, 0x5d, 0x5e, 0xa0, 0xca, 0x30, 0x5c, 0xef, 0x97, 0xb5, 0x44, 0x8a,
                0x1d, 0xb8, 0x45,
            ]
        );
        let (key, iv) = CipherAlg::Aes256.derive_key(HashAlg::Md5, &digest);
        assert_eq!(
            key,
            [
                0x20, 0x7f, 0xe5, 0x09, 0x9b, 0xc3, 0x20, 0xf9, 0x5b, 0x7a, 0x6f, 0x3b, 0x66,
                0x04, 0x29, 0xc7, 0xa8, 0xf3, 0x48, 0x6b, 0x7e, 0x69, 0x9f, 0x06, 0x02, 0x93,
                0x00, 0xf5, 0x03, 0x36, 0xdf, 0xde,
            ]
        );
        // Every AES width draws from the same expansion, and CBC starts with an
        // all-zero IV.
        let (short, _) = CipherAlg::Aes128.derive_key(HashAlg::Md5, &digest);
        assert_eq!(short, key[..16]);
        let (mid, _) = CipherAlg::Aes192.derive_key(HashAlg::Md5, &digest);
        assert_eq!(mid, key[..24]);
        assert_eq!(iv, vec![0u8; 16]);
    }

    #[test]
    fn a_short_hash_still_fills_rc4_and_sha2_keys_directly() {
        // RC4 is a stream cipher, so the first-`n`-bytes rule applies; a SHA-2
        // digest is long enough for AES-256 on its own.
        let digest = md5(b"password");
        let (rc4, iv) = CipherAlg::Rc4.derive_key(HashAlg::Md5, &digest);
        assert_eq!(rc4, digest);
        assert!(iv.is_empty());

        let sha = sha256(b"password");
        let (key, _) = CipherAlg::Aes256.derive_key(HashAlg::Sha256, &sha);
        assert_eq!(key, sha);
    }
}
