//! Unit tests for `winemu::crypto`'s CryptoAPI emulation.
//!
//! Kept out of `crypto.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

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

#[test]
fn crc32_matches_the_published_check_value() {
    // CRC-32/ISO-HDLC of "123456789".
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(0, b""), 0);
}

#[test]
fn crc32_continues_from_a_previous_result() {
    // `RtlComputeCrc32` is chained by feeding the last result back in.
    let first = crc32(0, b"1234");
    assert_eq!(crc32(first, b"56789"), crc32(0, b"123456789"));
    assert_ne!(crc32(0x1234_5678, b"123456789"), crc32(0, b"123456789"));
}
