//! The CryptoAPI half of `DllCall`, plus `RtlDecompressBuffer`.
//!
//! The algorithms are in [`super::super::crypto`] (CryptoAPI, RustCrypto-backed)
//! and [`super::super::bcrypt`] (CNG); this file is the `advapi32`/`ntdll`
//! surface those two answer.

use autoitv3_i18n::msg;
use autoitv3_runtime::value::Value;

use super::WindowsEmulation;
use super::*;
use super::super::*;

impl WindowsEmulation {
    /// `CryptCreateHash(hProv, algid, hKey, flags, phHash)`.
    pub(in crate::winemu) fn crypt_create_hash(&mut self, algid: &Value) -> Option<DllOutcome> {
        let Some(alg) = crate::winemu::crypto::HashAlg::from_algid(algid.to_int() as u32) else {
            if self.trace_dll {
                let algid = format!("{:#x}", algid.to_int());
                eprintln!(
                    "{}",
                    msg!(
                        "[winemu] CryptCreateHash: unsupported hash algid {algid}",
                        algid = algid
                    )
                );
            }
            return None;
        };
        let slot = Some(HashObject {
            alg,
            data: Vec::new(),
        });
        let handle = if let Some(i) = self.crypto.hashes.iter().position(|h| h.is_none()) {
            self.crypto.hashes[i] = slot;
            i as i64 + 1
        } else {
            self.crypto.hashes.push(slot);
            self.crypto.hashes.len() as i64
        };
        // The sample hands `phHash` a literal 0 and reads the handle out of
        // `$result[5]`, so the array slot is the only channel that matters.
        Some(DllOutcome::with(Value::Int(1), 4, Value::Int(handle)))
    }

    /// `CryptHashData(hHash, pbData, dwDataLen, flags)`.
    pub(in crate::winemu) fn crypt_hash_data(
        &mut self,
        handle: &Value,
        buffer: &Value,
        len: &Value,
    ) -> Option<DllOutcome> {
        let index = usize::try_from(handle.to_int()).ok()?.checked_sub(1)?;
        let len = len.to_int().max(0) as usize;
        let bytes = self.read_buffer(buffer, len)?;
        let slot = self.crypto.hashes.get_mut(index)?.as_mut()?;
        slot.data.extend_from_slice(&bytes);
        Some(DllOutcome::value(Value::Int(1)))
    }

    /// `CryptGetHashParam(hHash, param, pbData, pdwDataLen, flags)`.
    pub(in crate::winemu) fn crypt_get_hash_param(
        &mut self,
        handle: &Value,
        param: &Value,
        buffer: &Value,
    ) -> Option<DllOutcome> {
        let index = usize::try_from(handle.to_int()).ok()?.checked_sub(1)?;
        let alg = self.crypto.hashes.get(index)?.as_ref()?.alg;
        let data = self.crypto.hashes.get(index)?.as_ref()?.data.clone();
        match param.to_int() {
            // HP_HASHSIZE
            4 => {
                let size = Value::Int(alg.digest_len() as i64);
                self.write_buffer(buffer, &(alg.digest_len() as u32).to_le_bytes());
                Some(DllOutcome::with(Value::Int(1), 2, size))
            }
            // HP_HASHVAL
            2 => {
                let digest = alg.digest(&data);
                self.write_buffer(buffer, &digest);
                Some(DllOutcome::with(
                    Value::Int(1),
                    2,
                    Value::Binary(std::rc::Rc::new(digest)),
                ))
            }
            // HP_ALGID
            1 => Some(DllOutcome::with(Value::Int(1), 2, Value::Int(0))),
            _ => None,
        }
    }

    /// `CryptDeriveKey(hProv, algid, hBaseData, flags, phKey)`.
    pub(in crate::winemu) fn crypt_derive_key(&mut self, algid: &Value, base: &Value) -> Option<DllOutcome> {
        let Some(alg) = crate::winemu::crypto::CipherAlg::from_algid(algid.to_int() as u32) else {
            if self.trace_dll {
                let algid = format!("{:#x}", algid.to_int());
                eprintln!(
                    "{}",
                    msg!(
                        "[winemu] CryptDeriveKey: unsupported cipher algid {algid}",
                        algid = algid
                    )
                );
            }
            return None;
        };
        // The key material is the digest of the hash object handed in, run
        // through CryptoAPI's derivation for the requested algorithm.
        let index = usize::try_from(base.to_int()).ok()?.checked_sub(1)?;
        let (hash_alg, digest) = {
            let hash = self.crypto.hashes.get(index)?.as_ref()?;
            (hash.alg, hash.alg.digest(&hash.data))
        };
        let (key, iv) = alg.derive_key(hash_alg, &digest);
        let handle = if let Some(i) = self.crypto.keys.iter().position(|k| k.is_none()) {
            self.crypto.keys[i] = Some(KeyObject { alg, key, iv });
            i as i64 + 1
        } else {
            self.crypto.keys.push(Some(KeyObject { alg, key, iv }));
            self.crypto.keys.len() as i64
        };
        Some(DllOutcome::with(Value::Int(1), 4, Value::Int(handle)))
    }

    /// `CryptDecrypt(hKey, hHash, final, flags, pbData, pdwDataLen)`.
    pub(in crate::winemu) fn crypt_decrypt(
        &mut self,
        handle: &Value,
        buffer: &Value,
        len: &Value,
        final_block: bool,
    ) -> Option<DllOutcome> {
        let index = usize::try_from(handle.to_int()).ok()?.checked_sub(1)?;
        let key = self.crypto.keys.get(index)?.as_ref()?.clone();
        let len = len.to_int().max(0) as usize;
        let data = self.read_buffer(buffer, len)?;
        let mut plain = key.alg.apply(&key.key, &key.iv, &data);
        // CryptoAPI's block ciphers pad to the block size with PKCS#7, and the
        // final `CryptDecrypt` strips it — leaving it in would append up to a
        // block of 0x10 bytes to the plaintext.
        if final_block {
            if let Some(keep) = pkcs7_kept_len(&plain) {
                plain.truncate(keep);
            }
        }
        if !self.write_buffer(buffer, &plain) {
            return None;
        }
        Some(DllOutcome::with(
            Value::Int(1),
            5,
            Value::Int(plain.len() as i64),
        ))
    }

    /// `RtlDecompressBuffer(format, outBuf, outLen, inBuf, inLen, pOutLen)`.
    pub(in crate::winemu) fn rtl_decompress_buffer(
        &mut self,
        format: &Value,
        out_buf: &Value,
        out_len: &Value,
        in_buf: &Value,
        in_len: &Value,
    ) -> Option<DllOutcome> {
        if format.to_int() as u32 != compress::COMPRESSION_FORMAT_LZNT1 {
            return None;
        }
        let input = self.read_buffer(in_buf, in_len.to_int().max(0) as usize)?;
        let mut plain = compress::decompress(&input)?;
        let capacity = out_len.to_int().max(0) as usize;
        if capacity > 0 {
            plain.truncate(capacity);
        }
        if !self.write_buffer(out_buf, &plain) {
            return None;
        }
        Some(DllOutcome::with(
            Value::Int(0),
            5,
            Value::Int(plain.len() as i64),
        ))
    }
}

/// Decode a PKCS#7 pad length: `Some(n)` is the length without padding (used by
/// the final `CryptDecrypt`, where the CryptoAPI strips the pad), `None` when the
/// trailing block is not padding, in which case the data is passed through untouched).
fn pkcs7_kept_len(data: &[u8]) -> Option<usize> {
    let pad = *data.last()? as usize;
    if pad == 0 || pad > 16 || pad > data.len() {
        return None;
    }
    if data[data.len() - pad..].iter().all(|b| *b as usize == pad) {
        Some(data.len() - pad)
    } else {
        None
    }
}