//! `bcrypt.dll` (Windows CNG), emulated.
//!
//! The newer AutoIt crypto UDFs go through CNG rather than the old CryptoAPI:
//! they open an algorithm provider, use it for a hash or a key, and read object
//! sizes back through `BCryptGetProperty`. The path that needs this in practice
//! derives a key from a password (SHA-256 HMAC, PBKDF2, AES-CBC) and then
//! RSA-decrypts a payload, so those are the operations implemented here.
//!
//! Handles are indices into tables — a script only ever compares them against
//! zero and hands them back, so a real pointer would add nothing. The algorithms
//! come from RustCrypto; the CryptoAPI half of the emulation keeps its own
//! primitives in [`crate::winemu::crypto`].
//!
//! **Deliberate approximations**
//!
//! * `BCryptGenRandom` fills the buffer from a seeded generator, so an analysis
//!   run stays reproducible. A script that seeds nothing gets the same bytes
//!   every run, which is what the deterministic profile promises.
//! * Only the properties a script can actually read are answered; anything else
//!   reports failure rather than inventing a value.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use rsa::traits::PublicKeyParts;
use hmac::{Hmac, Mac};
use rsa::BigUint;

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes192CbcEnc = cbc::Encryptor<aes::Aes192>;
type Aes192CbcDec = cbc::Decryptor<aes::Aes192>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// An algorithm `BCryptOpenAlgorithmProvider` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    Aes,
    Rsa,
}

impl Alg {
    /// The algorithm an `pszAlgId` names, case-insensitively.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_uppercase().as_str() {
            "MD5" => Self::Md5,
            "SHA1" => Self::Sha1,
            "SHA256" => Self::Sha256,
            "SHA384" => Self::Sha384,
            "SHA512" => Self::Sha512,
            "AES" => Self::Aes,
            "RSA" => Self::Rsa,
            _ => return None,
        })
    }

    /// Digest length in bytes, for `BCRYPT_HASH_LENGTH`.
    fn digest_len(self) -> Option<usize> {
        Some(match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
            _ => return None,
        })
    }

    /// Input block length in bytes, for `BCRYPT_HASH_BLOCK_LENGTH`.
    fn block_len(self) -> Option<usize> {
        Some(match self {
            Self::Md5 | Self::Sha1 | Self::Sha256 => 64,
            Self::Sha384 | Self::Sha512 => 128,
            _ => return None,
        })
    }

    /// Whether the algorithm is the (only) hash family CNG is asked for here.
    fn is_hash(self) -> bool {
        self.digest_len().is_some()
    }
}

/// A value `BCryptGetProperty` can hand back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Property {
    /// A little-endian `ULONG`.
    Ulong(u32),
    /// `BCRYPT_KEY_LENGTHS_STRUCT`: min, max and step, in bits.
    KeyLengths {
        min: u32,
        max: u32,
        increment: u32,
    },
}

impl Property {
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Ulong(v) => v.to_le_bytes().to_vec(),
            Self::KeyLengths {
                min,
                max,
                increment,
            } => {
                let mut out = Vec::with_capacity(12);
                out.extend_from_slice(&min.to_le_bytes());
                out.extend_from_slice(&max.to_le_bytes());
                out.extend_from_slice(&increment.to_le_bytes());
                out
            }
        }
    }
}

/// One opened algorithm provider.
#[derive(Debug, Clone)]
struct Provider {
    alg: Alg,
    /// `BCRYPT_ALG_HANDLE_HMAC_FLAG`: hashes made from it are HMACs.
    hmac: bool,
    /// `BCRYPT_CHAINING_MODE`, which `BCryptSetProperty` chooses.
    chaining_cbc: bool,
}

/// A hash (or HMAC) being fed data.
#[derive(Debug, Clone)]
struct HashObject {
    alg: Alg,
    /// `Some` when the provider had the HMAC flag and `BCryptCreateHash` was
    /// given a secret.
    hmac_key: Option<Vec<u8>>,
    data: Vec<u8>,
}

/// A key object, symmetric or RSA.
enum KeyObject {
    /// A block cipher key. The chaining mode lives on the provider it came from;
    /// only CBC is reachable from these scripts.
    Symmetric { secret: Vec<u8> },
    Rsa(Box<rsa::RsaPrivateKey>),
}

impl std::fmt::Debug for KeyObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Symmetric { secret } => write!(f, "Symmetric({} bytes)", secret.len()),
            Self::Rsa(key) => write!(f, "Rsa({} bits)", key.n().bits()),
        }
    }
}

impl KeyObject {
    /// Bytes a `BCryptEncrypt`/`BCryptDecrypt` output would occupy, so the first
    /// (sizing) call can answer `pcbResult`.
    fn output_len(&self, input: usize, padding: bool) -> Option<usize> {
        match self {
            Self::Symmetric { secret } => {
                let block = block_size_for(secret.len())?;
                Some(if padding {
                    input.div_ceil(block) * block
                } else {
                    input
                })
            }
            Self::Rsa(key) => Some(key.size()),
        }
    }
}

/// The block size of an AES key of this length, in bytes.
fn block_size_for(key_len: usize) -> Option<usize> {
    matches!(key_len, 16 | 24 | 32).then_some(16)
}

/// `bcrypt.dll` objects: providers, hashes and keys.
#[derive(Default)]
pub struct BcryptState {
    providers: Vec<Option<Provider>>,
    hashes: Vec<Option<HashObject>>,
    keys: Vec<Option<KeyObject>>,
    /// Deterministic entropy for `BCryptGenRandom` (see the module docs).
    rng: u64,
}

impl BcryptState {
    /// `BCryptOpenAlgorithmProvider`. `None` is "no such algorithm".
    pub fn open_provider(&mut self, alg_name: &str, hmac: bool) -> Option<i64> {
        let alg = Alg::from_name(alg_name)?;
        let provider = Provider {
            alg,
            hmac,
            chaining_cbc: false,
        };
        Some(push(&mut self.providers, Some(provider)))
    }

    /// `BCryptCloseAlgorithmProvider`.
    pub fn close_provider(&mut self, handle: i64) -> bool {
        take(&mut self.providers, handle)
    }

    /// `BCryptGetProperty`.
    pub fn property(&self, handle: i64, name: &str) -> Option<Property> {
        let upper = name.trim().to_ascii_uppercase();
        // Hash handles answer hash properties, key handles key properties; the
        // script asks the right one, so try both tables.
        if let Some(hash) = get(&self.hashes, handle) {
            return match upper.as_str() {
                "HASHDIGESTLENGTH" | "BCRYPT_HASH_LENGTH" => {
                    hash.alg.digest_len().map(|n| Property::Ulong(n as u32))
                }
                "HASHBLOCKLENGTH" | "BCRYPT_HASH_BLOCK_LENGTH" => {
                    hash.alg.block_len().map(|n| Property::Ulong(n as u32))
                }
                "ALGORITHMNAME" | "BCRYPT_ALGORITHM_NAME" => {
                    Some(Property::Ulong(hash.alg as u32))
                }
                _ => None,
            };
        }
        let provider = get(&self.providers, handle)?;
        match upper.as_str() {
            "HASHDIGESTLENGTH" | "BCRYPT_HASH_LENGTH" => {
                provider.alg.digest_len().map(|n| Property::Ulong(n as u32))
            }
            "HASHBLOCKLENGTH" | "BCRYPT_HASH_BLOCK_LENGTH" => {
                provider.alg.block_len().map(|n| Property::Ulong(n as u32))
            }
            "KEYLENGTHS" | "BCRYPT_KEY_LENGTHS" => Some(match provider.alg {
                // The values CNG reports: AES steps 64 bits from 128 to 256,
                // RSA from 512 to 16384 bits (the Windows default provider).
                Alg::Aes => Property::KeyLengths {
                    min: 128,
                    max: 256,
                    increment: 64,
                },
                Alg::Rsa => Property::KeyLengths {
                    min: 512,
                    max: 16384,
                    increment: 64,
                },
                _ => return None,
            }),
            _ => None,
        }
    }

    /// `BCryptSetProperty`. Only the chaining mode is reachable from scripts;
    /// a value that is not a known constant leaves the provider unchanged and
    /// reports failure, so the script's own error path runs.
    pub fn set_property(&mut self, handle: i64, name: &str, value: &str) -> bool {
        if !name.trim().eq_ignore_ascii_case("ChainingMode") {
            return false;
        }
        let Some(provider) = get_mut(&mut self.providers, handle) else {
            return false;
        };
        // The constant is passed as the string `ChainingModeCBC` (or the
        // `...ECB`/`...CFB` siblings); only CBC is implemented, anything else
        // must not be silently treated as CBC.
        if value.eq_ignore_ascii_case("ChainingModeCBC") {
            provider.chaining_cbc = true;
            true
        } else if value.eq_ignore_ascii_case("ChainingModeECB") {
            provider.chaining_cbc = false;
            true
        } else {
            false
        }
    }

    /// `BCryptCreateHash`. `secret` is the HMAC key, when the provider asked for
    /// one; an HMAC provider without a secret hashes plainly, as CNG does.
    pub fn create_hash(&mut self, provider: i64, secret: Option<&[u8]>) -> Option<i64> {
        let provider = get(&self.providers, provider)?;
        let alg = provider.alg;
        // A secret only means anything to a provider opened with the HMAC flag;
        // CNG hashes plainly otherwise, and so does this.
        let key = provider
            .hmac
            .then(|| secret.filter(|s| !s.is_empty()))
            .flatten()
            .map(|s| s.to_vec());
        Some(push(
            &mut self.hashes,
            Some(HashObject {
                alg,
                hmac_key: key,
                data: Vec::new(),
            }),
        ))
    }

    /// `BCryptHashData`.
    pub fn hash_data(&mut self, hash: i64, data: &[u8]) -> bool {
        match get_mut(&mut self.hashes, hash) {
            Some(slot) => {
                slot.data.extend_from_slice(data);
                true
            }
            None => false,
        }
    }

    /// `BCryptFinishHash`: the digest so far. CNG lets a hash be reused after
    /// this; these scripts do not, so the state is left as it was.
    pub fn finish_hash(&self, hash: i64) -> Option<Vec<u8>> {
        let slot = get(&self.hashes, hash)?;
        let data = slot.data.as_slice();
        Some(match (&slot.hmac_key, slot.alg) {
            (Some(key), alg) => hmac(alg, key, data)?,
            (None, alg) => digest(alg, data)?,
        })
    }

    /// `BCryptDestroyHash`.
    pub fn destroy_hash(&mut self, hash: i64) -> bool {
        take(&mut self.hashes, hash)
    }

    /// `BCryptGenerateSymmetricKey`: the secret is the key material itself.
    pub fn generate_symmetric_key(&mut self, provider: i64, secret: &[u8]) -> Option<i64> {
        let alg = get(&self.providers, provider)?.alg;
        if alg != Alg::Aes || block_size_for(secret.len()).is_none() {
            return None;
        }
        Some(push(
            &mut self.keys,
            Some(KeyObject::Symmetric {
                secret: secret.to_vec(),
            }),
        ))
    }

    /// `BCryptImportKeyPair` for a `BCRYPT_RSAPRIVATE_BLOB`.
    ///
    /// The blob is the CNG layout: a six-word header (magic, bit length, and the
    /// byte lengths of the components) followed by public exponent, modulus,
    /// primes, CRT exponents and coefficient, all big-endian.
    pub fn import_rsa_private_key(&mut self, provider: i64, blob: &[u8]) -> Option<i64> {
        if get(&self.providers, provider)?.alg != Alg::Rsa {
            return None;
        }
        let key = parse_rsa_private_blob(blob)?;
        Some(push(&mut self.keys, Some(KeyObject::Rsa(Box::new(key)))))
    }

    /// `BCryptDestroyKey`.
    pub fn destroy_key(&mut self, key: i64) -> bool {
        take(&mut self.keys, key)
    }

    /// `BCryptDeriveKeyPBKDF2`, using the provider's hash.
    pub fn derive_pbkdf2(
        &self,
        provider: i64,
        password: &[u8],
        salt: &[u8],
        iterations: u64,
        len: usize,
    ) -> Option<Vec<u8>> {
        let alg = get(&self.providers, provider)?.alg;
        if !alg.is_hash() || iterations == 0 || len == 0 {
            return None;
        }
        let iterations = u32::try_from(iterations).ok()?;
        let mut out = vec![0u8; len];
        match alg {
            Alg::Sha1 => pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, salt, iterations, &mut out),
            Alg::Sha256 => pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password, salt, iterations, &mut out),
            Alg::Sha384 => pbkdf2::pbkdf2_hmac::<sha2::Sha384>(password, salt, iterations, &mut out),
            Alg::Sha512 => pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password, salt, iterations, &mut out),
            Alg::Md5 => pbkdf2::pbkdf2_hmac::<md5::Md5>(password, salt, iterations, &mut out),
            _ => return None,
        }
        Some(out)
    }

    /// `BCryptEncrypt`/`BCryptDecrypt`.
    ///
    /// `iv` is the per-call IV for a block cipher (ignored for RSA), `padding`
    /// is the PKCS#7 block padding `BCRYPT_BLOCK_PADDING` asks for.
    pub fn crypt(
        &self,
        key: i64,
        data: &[u8],
        iv: Option<&[u8]>,
        encrypt: bool,
        padding: bool,
    ) -> Option<Vec<u8>> {
        match get(&self.keys, key)? {
            KeyObject::Symmetric { secret } => {
                if secret.len() != 32 && secret.len() != 24 && secret.len() != 16 {
                    return None;
                }
                if padding {
                    let iv = iv.filter(|v| v.len() == 16).unwrap_or(&[0u8; 16]);
                    if encrypt {
                        Some(aes_cbc_encrypt(secret, iv, data)?)
                    } else {
                        Some(aes_cbc_decrypt(secret, iv, data)?)
                    }
                } else {
                    // Unpadded CBC is not reachable from these scripts; refusing
                    // beats returning a block the script would misread.
                    None
                }
            }
            KeyObject::Rsa(key) => {
                if encrypt {
                    return None;
                }
                key.decrypt(rsa::Pkcs1v15Encrypt, data).ok()
            }
        }
    }

    /// The size `BCryptEncrypt`/`BCryptDecrypt` would return, for the sizing
    /// call whose output buffer is null.
    pub fn crypt_output_len(&self, key: i64, input: usize, padding: bool) -> Option<usize> {
        get(&self.keys, key)?.output_len(input, padding)
    }

    /// `BCryptGenRandom`, from the deterministic generator (see module docs).
    pub fn random(&mut self, len: usize) -> Vec<u8> {
        // xorshift64*, seeded on first use so a run is reproducible.
        if self.rng == 0 {
            self.rng = 0x2545_F491_4F6C_DD1D;
        }
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            self.rng ^= self.rng >> 12;
            self.rng ^= self.rng << 25;
            self.rng ^= self.rng >> 27;
            out.extend_from_slice(&self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/// The digest of `data` under `alg`.
fn digest(alg: Alg, data: &[u8]) -> Option<Vec<u8>> {
    use sha2::Digest;
    Some(match alg {
        Alg::Md5 => md5::Md5::digest(data).to_vec(),
        Alg::Sha1 => sha1::Sha1::digest(data).to_vec(),
        Alg::Sha256 => sha2::Sha256::digest(data).to_vec(),
        Alg::Sha384 => sha2::Sha384::digest(data).to_vec(),
        Alg::Sha512 => sha2::Sha512::digest(data).to_vec(),
        _ => return None,
    })
}

/// One HMAC, with the hash spelled out (a generic form needs HMAC trait bounds
/// that are not worth writing out five times).
macro_rules! hmac_of {
    ($hash:ty, $key:expr, $data:expr) => {{
        let mut mac =
            Hmac::<$hash>::new_from_slice($key).expect("HMAC accepts a key of any length");
        mac.update($data);
        mac.finalize().into_bytes().to_vec()
    }};
}

/// HMAC of `data` under `key` with the hash `alg` names.
fn hmac(alg: Alg, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    Some(match alg {
        Alg::Md5 => hmac_of!(md5::Md5, key, data),
        Alg::Sha1 => hmac_of!(sha1::Sha1, key, data),
        Alg::Sha256 => hmac_of!(sha2::Sha256, key, data),
        Alg::Sha384 => hmac_of!(sha2::Sha384, key, data),
        Alg::Sha512 => hmac_of!(sha2::Sha512, key, data),
        _ => return None,
    })
}


fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let out = match key.len() {
        16 => Aes128CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data),
        24 => Aes192CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data),
        32 => Aes256CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data),
        _ => return None,
    };
    Some(out)
}

fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let out = match key.len() {
        16 => Aes128CbcDec::new(key.into(), iv.into()).decrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data),
        24 => Aes192CbcDec::new(key.into(), iv.into()).decrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data),
        32 => Aes256CbcDec::new(key.into(), iv.into()).decrypt_padded_vec_mut::<cbc::cipher::block_padding::Pkcs7>(data),
        _ => return None,
    };
    out.ok()
}

/// Read a `BCRYPT_RSAKEY_BLOB` and build the key from its components.
///
/// The blob is a six-word header — magic, bit length, and the byte lengths of
/// the public exponent, modulus, prime1 and prime2 — followed by those
/// components big-endian, then the CRT exponents and coefficient, then the
/// private exponent.
///
/// A blob built by an AutoIt UDF may stop right after `q` (only prime1/prime2
/// present), because p, q and e are enough to rebuild the rest; `from_p_q` does
/// exactly that, so both shapes are accepted here.
fn parse_rsa_private_blob(blob: &[u8]) -> Option<rsa::RsaPrivateKey> {
    if blob.len() < 24 {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(blob[i * 4..i * 4 + 4].try_into().unwrap()) as usize;
    // `RSA2` is a private blob, `RSA1` a public one; only the former decrypts.
    if word(0) != 0x3241_5352 {
        return None;
    }
    let (exp_len, mod_len, p_len, q_len) = (word(2), word(3), word(4), word(5));
    if exp_len == 0 || mod_len == 0 || p_len == 0 || q_len == 0 {
        return None;
    }
    let (exp_at, mod_at) = (24, 24 + exp_len);
    let (p_at, q_at) = (mod_at + mod_len, mod_at + mod_len + p_len);
    let q_end = q_at + q_len;
    if blob.len() < q_end {
        return None;
    }
    let e = BigUint::from_bytes_be(&blob[exp_at..mod_at]);
    let n = BigUint::from_bytes_be(&blob[mod_at..p_at]);
    let p = BigUint::from_bytes_be(&blob[p_at..q_at]);
    let q = BigUint::from_bytes_be(&blob[q_at..q_end]);
    // Exponent1, exponent2, coefficient, then the private exponent.
    let d_at = q_end + 2 * p_len + q_len;
    if blob.len() >= d_at + mod_len {
        let d = BigUint::from_bytes_be(&blob[d_at..d_at + mod_len]);
        let key = rsa::RsaPrivateKey::from_components(n, e, d, vec![p, q]).ok()?;
        key.validate().ok()?;
        Some(key)
    } else {
        rsa::RsaPrivateKey::from_p_q(p, q, e).ok()
    }
}

// ---------------------------------------------------------------------------
// Handle tables
// ---------------------------------------------------------------------------

/// Store `value` in the first free slot, or append; returns its 1-based handle.
fn push<T>(table: &mut Vec<Option<T>>, value: Option<T>) -> i64 {
    if let Some(i) = table.iter().position(|slot| slot.is_none()) {
        table[i] = value;
        i as i64 + 1
    } else {
        table.push(value);
        table.len() as i64
    }
}

fn get<T>(table: &[Option<T>], handle: i64) -> Option<&T> {
    if handle < 1 {
        return None;
    }
    table.get(handle as usize - 1)?.as_ref()
}

fn get_mut<T>(table: &mut [Option<T>], handle: i64) -> Option<&mut T> {
    if handle < 1 {
        return None;
    }
    table.get_mut(handle as usize - 1)?.as_mut()
}

fn take<T>(table: &mut [Option<T>], handle: i64) -> bool {
    if handle < 1 {
        return false;
    }
    match table.get_mut(handle as usize - 1) {
        Some(slot) if slot.is_some() => {
            *slot = None;
            true
        }
        _ => false,
    }
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls the file back in as a module, so they can reach private state.
#[cfg(test)]
#[path = "../../tests/unit/winemu_bcrypt.rs"]
mod tests;
