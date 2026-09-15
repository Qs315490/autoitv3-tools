//! Unit tests for `winemu::bcrypt` — known-answer tests for the CNG surface.
//!
//! Reached through `#[path]` from `src/winemu/bcrypt.rs`. The values are the
//! published vectors (FIPS 180-4 for SHA-256, RFC 4231 for HMAC, RFC 7914 for
//! PBKDF2), so a wrong primitive shows up here rather than as a script that
//! quietly derives the wrong key.

use super::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A provider for `alg`, with the HMAC flag when asked.
fn provider(state: &mut BcryptState, alg: &str, hmac: bool) -> i64 {
    state.open_provider(alg, hmac).expect("known algorithm")
}

#[test]
fn sha256_matches_the_published_vector() {
    let mut state = BcryptState::default();
    let p = provider(&mut state, "SHA256", false);
    let h = state.create_hash(p, None).expect("hash");
    assert!(state.hash_data(h, b"abc"));
    let digest = state.finish_hash(h).expect("digest");
    assert_eq!(
        hex(&digest),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn hash_data_accumulates_across_calls() {
    let mut state = BcryptState::default();
    let p = provider(&mut state, "sha256", false);
    let h = state.create_hash(p, None).expect("hash");
    state.hash_data(h, b"ab");
    state.hash_data(h, b"c");
    // Same digest as the one-shot case above: hashing is streaming.
    assert_eq!(
        hex(&state.finish_hash(h).expect("digest")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn hmac_sha256_matches_rfc4231_case_1() {
    let mut state = BcryptState::default();
    let key = [0x0bu8; 20];
    let p = provider(&mut state, "SHA256", true);
    let h = state.create_hash(p, Some(&key)).expect("hmac");
    state.hash_data(h, b"Hi There");
    assert_eq!(
        hex(&state.finish_hash(h).expect("digest")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn a_secret_on_a_plain_provider_hashes_plainly() {
    // CNG only honours a secret when the provider was opened with the HMAC
    // flag; otherwise the hash is not keyed.
    let mut state = BcryptState::default();
    let p = provider(&mut state, "SHA256", false);
    let h = state.create_hash(p, Some(b"key")).expect("hash");
    state.hash_data(h, b"abc");
    assert_eq!(
        hex(&state.finish_hash(h).expect("digest")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn pbkdf2_hmac_sha256_matches_the_published_vector() {
    let mut state = BcryptState::default();
    let p = provider(&mut state, "SHA256", false);
    let derived = state
        .derive_pbkdf2(p, b"password", b"salt", 1, 32)
        .expect("pbkdf2");
    assert_eq!(
        hex(&derived),
        "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
    );
}

#[test]
fn aes_cbc_round_trips_through_encrypt_and_decrypt() {
    let mut state = BcryptState::default();
    let p = provider(&mut state, "AES", false);
    let key = [7u8; 32];
    let iv = [9u8; 16];
    let k = state.generate_symmetric_key(p, &key).expect("key");
    let message = b"the quick brown fox";
    let sealed = state
        .crypt(k, message, Some(&iv), true, true)
        .expect("encrypt");
    assert_eq!(sealed.len(), 32, "PKCS#7 pads to the next block");
    let opened = state
        .crypt(k, &sealed, Some(&iv), false, true)
        .expect("decrypt");
    assert_eq!(opened, message);
}

#[test]
fn a_destroyed_hash_or_key_is_no_longer_usable() {
    let mut state = BcryptState::default();
    let p = provider(&mut state, "SHA256", false);
    let h = state.create_hash(p, None).expect("hash");
    assert!(state.destroy_hash(h));
    assert!(!state.hash_data(h, b"abc"));
    assert!(state.finish_hash(h).is_none());
}

#[test]
fn gen_random_is_deterministic_but_not_repeating() {
    // The analysis profile promises reproducible bytes, so the same run has to
    // hand back the same stream; a run must still not return one value forever.
    let mut a = BcryptState::default();
    let mut b = BcryptState::default();
    assert_eq!(a.random(32), b.random(32), "two runs, same stream");
    // The stream advances rather than repeating one value.
    assert_ne!(a.random(16), a.random(16));
}

#[test]
fn only_real_algorithms_open() {
    let mut state = BcryptState::default();
    assert!(state.open_provider("no-such-alg", false).is_none());
    assert!(state.open_provider("AES", false).is_some());
}

#[test]
fn an_rsa_blob_that_is_not_a_private_key_is_rejected() {
    let mut state = BcryptState::default();
    let p = provider(&mut state, "RSA", false);
    // A public blob (magic `RSA1`) must not decrypt.
    let mut blob = vec![0u8; 32];
    blob[0..4].copy_from_slice(b"RSA1");
    assert!(state.import_rsa_private_key(p, &blob).is_none());
    assert!(state.import_rsa_private_key(p, b"").is_none());
}

#[test]
fn key_lengths_report_the_cng_ranges() {
    let mut state = BcryptState::default();
    let aes = provider(&mut state, "AES", false);
    assert_eq!(
        state.property(aes, "KeyLengths"),
        Some(Property::KeyLengths {
            min: 128,
            max: 256,
            increment: 64
        })
    );
    let rsa = provider(&mut state, "RSA", false);
    assert_eq!(
        state.property(rsa, "KeyLengths"),
        Some(Property::KeyLengths {
            min: 512,
            max: 16384,
            increment: 64
        })
    );
}

#[test]
fn the_chaining_mode_is_only_set_for_a_known_constant() {
    let mut state = BcryptState::default();
    let aes = provider(&mut state, "AES", false);
    assert!(state.set_property(aes, "ChainingMode", "ChainingModeCBC"));
    assert!(!state.set_property(aes, "ChainingMode", "ChainingModeGCM"));
    assert!(!state.set_property(aes, "NotAProperty", "x"));
}
