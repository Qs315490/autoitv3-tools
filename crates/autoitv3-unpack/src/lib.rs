//! Reading an AutoIt build's payloads back out of the build.
//!
//! Two different things hide inside an AutoIt executable, and this crate reads
//! both:
//!
//! * the **compiled script** `aut2exe` embedded — the source the program was
//!   built from, tokenised and compressed. The [`script`] module locates the
//!   `AU3!EA05`/`AU3!EA06` chunk, decrypts and decompresses it, and
//!   deassembles the token stream back into `.au3` text, so a build whose
//!   source is lost can be read back.
//! * a **resource-packed payload** some builds add on top, where the image's
//!   `RT_RCDATA` resources hold an encrypted file set that only the program
//!   itself can read. [`unpack`] decodes that without running anything.
//!
//! The resource-packed half is described below, because it needs the longer
//! explanation; [`script`] documents its own format where it is defined.
//!
//! A build that keeps its payload in the image's `RT_RCDATA` resources can
//! only be read back by the program that packed it, which is a problem when
//! the script is missing and the resource names are randomised per build.
//! The scheme this decodes, stated generically, is:
//!
//! ```text
//! loader key   = SHA1(member1) · SHA1(member2) · SHA1(member3), as upper-case
//!                hex — so the root key is derived from the payload itself
//! PswDict      = AES-256-CBC(loader, CryptDeriveKey(MD5(loader key)))
//! member_i     = RC4(member_i, MD5(prefix of PswDict)); a hex string
//! ```
//!
//! then three further layers (`Data`/`Hash`/`Psw` split off the hex text, an
//! arithmetically self-decoded hash blob used as a second key table, an
//! AES-192 password step) before the final double AES-128 + AES-256 decrypt,
//! whose result is verified against the stored SHA-1.
//!
//! The resource-packer half is deliberately *not* a general AutoIt facility:
//! it is one packer's format, kept in its own crate so the interpreter stays
//! generic. It exists because the same format is reused across builds with
//! randomised resource names, and `au3 unpack` should work on any of them
//! without needing the obfuscated script — let alone the multi-megabyte `.exe`.
//!
//! Everything below follows the packing program's own logic, including that
//! its `Dec` reads *hexadecimal* and that its `StringMid` clamps at the end of
//! a string instead of failing.

pub mod script;

use std::fmt;
use std::path::{Path, PathBuf};

use autoitv3_i18n::{msg, tr};
use autoitv3_platform::{CipherAlg, HashAlg, PeImage, Selector};

pub use script::{CompiledScript, FileKind, ScriptFile, ScriptVersion};

/// How many blocks the format splits its payload into.
const BLOCKS: usize = 3;
/// `PswDictLen`: the decrypted dictionary is always this many bytes.
const PSW_DICT_LEN: usize = 1024;
/// `PasswordPswMaxLen`: the longest password the format will build.
const PASSWORD_MAX_LEN: usize = 128;

/// Why unpacking failed.
#[derive(Debug)]
pub enum Error {
    /// No resource set looked like a packed payload.
    NotAPackage,
    /// No `AU3!EA05`/`AU3!EA06` chunk was found, so there is no compiled
    /// script to read.
    NoCompiledScript,
    /// The image is packed (UPX), so its real content is inside the packed
    /// data and nothing can be read from it here.
    Packed(String),
    /// A chunk was decoded but it carries no script entry — only embedded
    /// payloads, say.
    NoScript,
    /// A resource set was found but a stage rejected its data.
    BadData(String),
    /// The index spec the caller asked for is not usable.
    BadIndex(String),
    /// A path could not be read.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAPackage => write!(
                f,
                "{}",
                tr("no packed payload found (expected a derived-key loader plus \
                 three stream-cipher members among the resources)")
            ),
            Error::NoCompiledScript => write!(
                f,
                "{}",
                tr("no compiled script found (expected an AU3!EA05 or AU3!EA06 \
                 chunk, a resource named SCRIPT, or a raw chunk)")
            ),
            Error::NoScript => write!(
                f,
                "{}",
                tr("the build carries no script entry (only embedded payloads)")
            ),
            Error::Packed(packer) => write!(
                f,
                "{}",
                msg!(
                    "the image is {packer}-packed: its script and resources are inside                      the packed data, so unpack the stub first (for example `upx -d FILE`)                      and read the result",
                    packer = packer
                )
            ),
            Error::BadData(why) => write!(
                f,
                "{}",
                msg!("the packed payload is malformed: {why}", why = why)
            ),
            Error::BadIndex(why) => write!(
                f,
                "{}",
                msg!("invalid index spec: {why}", why = why)
            ),
            // (kept distinct from the inner message so it reads as one line)
            Error::Io(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The resources a package was assembled from, by name.
#[derive(Debug, Clone)]
pub struct Package {
    /// The resource the password dictionary is encrypted into.
    pub loader: String,
    /// The three payload blocks, in the order the format indexes them.
    pub members: [String; BLOCKS],
}

/// A decoded package.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// Which resources it came out of.
    pub package: Package,
    /// The concatenated plaintext, before it is split into entries.
    pub text: String,
    /// The configuration entries: `text` split on its own three-character
    /// separator, which the format stores in the first three bytes.
    pub entries: Vec<String>,
}

// ---------------------------------------------------------------------------
// Reading the resources
// ---------------------------------------------------------------------------

/// Every resource-shaped file in `dir`, as `(name, bytes)`.
///
/// Recognises the names `AutoIt3Wrapper_Res_File_Add` stages: `__NAME`,
/// `__Res64/NAME` and `__ResImage/_NAME`. Files whose name carries no staging
/// prefix are included under their bare name, because a plain resource dump
/// names them that way.
pub fn candidates_from_dir(dir: impl AsRef<Path>) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let dir = dir.as_ref();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut push = |name: String, path: PathBuf| {
        if out.iter().any(|(n, _)| n == &name) {
            return;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            out.push((name, bytes));
        }
    };
    // `AutoIt3Wrapper` stages the resources with a `__` prefix. When that
    // layout is present it *is* the candidate set — anything else in the
    // directory (the script, unrelated programs) would only slow the search
    // down and give the combination test more ways to be fooled.
    let staged = collect(dir, |name| name.starts_with("__"))
        .into_iter()
        .chain(collect(&dir.join("__Res64"), |_| true))
        .chain(collect(&dir.join("__ResImage"), |_| true))
        .collect::<Vec<_>>();
    if !staged.is_empty() {
        for (path, name) in staged {
            let logical = name.trim_start_matches("__").trim_start_matches('_');
            push(logical.to_string(), path);
        }
        return Ok(out);
    }
    // Otherwise assume the directory is a plain resource dump.
    for (path, name) in collect(dir, |_| true) {
        push(name, path);
    }
    Ok(out)
}

/// Regular files in `dir` whose name passes `keep`, as `(path, file name)`.
fn collect(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, String)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            keep(&name).then_some((p, name))
        })
        .collect();
    out.sort();
    out
}

/// Every `RT_RCDATA` resource of a PE image, as `(name, bytes)`.
///
/// Numeric resource ids are rendered as their decimal string, since that is
/// what `FindResourceW` would have been called with.
pub fn candidates_from_image(path: impl AsRef<Path>) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let path = path.as_ref();
    let image = PeImage::load(path).map_err(Error::Io)?;
    // A packed image's resource directory is the packer's, not the build's:
    // whatever the script embedded lives inside the packed data, so say that
    // instead of reporting the (empty) list as "no payload here".
    if let Some(packer) = image.packer {
        return Err(Error::Packed(packer.to_string()));
    }
    let mut out = Vec::new();
    for r in image.resources.iter().filter(|r| {
        // RT_RCDATA is 10; anything else is an icon or a version block.
        r.type_sel.id == Some(10)
            || r.type_sel
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case("RCDATA"))
    }) {
        let name = r
            .name_sel
            .name
            .clone()
            .or_else(|| r.name_sel.id.map(|i| i.to_string()))
            .unwrap_or_default();
        out.push((name, r.data.clone()));
    }
    Ok(out)
}

/// Write `resources` under `dir`, one file each, grouped by resource type.
///
/// This is the read-back the `#AutoIt3Wrapper_Res_File_Add` files are for: a
/// build keeps every added file as a resource under the name that directive
/// gave it, so writing them out hands those files back without the build's
/// script being involved. A name that is not usable as a file name (a path, a
/// colon, an empty numeric id) is flattened, and a name that collides with one
/// already written in the same type directory gets a `.N` suffix.
///
/// Returns the paths written, in order.
pub fn write_resources(
    dir: impl AsRef<Path>,
    resources: &[(String, String, Vec<u8>)],
) -> Result<Vec<PathBuf>, Error> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
    let mut used: Vec<(String, String)> = Vec::new();
    let mut written: Vec<PathBuf> = Vec::new();
    for (index, (kind, name, bytes)) in resources.iter().enumerate() {
        let kind_dir = sanitize_component(kind);
        let sub = dir.join(&kind_dir);
        std::fs::create_dir_all(&sub).map_err(|e| Error::Io(e.to_string()))?;
        let file = sub.join(unique_file_name(&mut used, &kind_dir, name, index + 1));
        std::fs::write(&file, bytes).map_err(|e| Error::Io(e.to_string()))?;
        written.push(file);
    }
    Ok(written)
}

/// A resource name turned into a file name that is unique within `used`.
fn unique_file_name(
    used: &mut Vec<(String, String)>,
    kind_dir: &str,
    name: &str,
    index: usize,
) -> String {
    let cleaned = sanitize_component(name);
    let base = if cleaned == "UNKNOWN" {
        format!("resource_{index}")
    } else {
        cleaned
    };
    let mut candidate = base.clone();
    let mut n = 2;
    while used
        .iter()
        .any(|(k, u)| k.eq_ignore_ascii_case(kind_dir) && u.eq_ignore_ascii_case(&candidate))
    {
        candidate = format!("{base}.{n}");
        n += 1;
    }
    used.push((kind_dir.to_string(), candidate.clone()));
    candidate
}

/// A string reduced to what one directory or file name can carry.
fn sanitize_component(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "UNKNOWN".to_string()
    } else {
        cleaned
    }
}

/// Every resource of a PE image, as (type, name, bytes).
///
/// Where `candidates_from_image` keeps only the RT_RCDATA entries the
/// packed-payload search cares about, this is what `au3 unpack` writes out:
/// every type the image declares, so the files a build added with
/// `#AutoIt3Wrapper_Res_File_Add` come back next to its icons, manifest and
/// version block. The type is the directory its entry belongs in.
pub fn resources_from_image(
    path: impl AsRef<Path>,
) -> Result<Vec<(String, String, Vec<u8>)>, Error> {
    let path = path.as_ref();
    let image = PeImage::load(path).map_err(Error::Io)?;
    if let Some(packer) = image.packer {
        return Err(Error::Packed(packer.to_string()));
    }
    Ok(image
        .resources
        .iter()
        .map(|r| {
            (
                type_dir_name(&r.type_sel),
                resource_name(&r.name_sel),
                r.data.clone(),
            )
        })
        .collect())
}

/// The directory a resource type is written under.
///
/// The names are Windows' own — RT_RCDATA is 10, and the RT_ prefix is
/// dropped — so a tree reads like RCDATA/SCRIPT, ICON/1, MANIFEST/1. A type
/// with no name of its own keeps its number rather than being folded into
/// something wrong.
fn type_dir_name(selector: &Selector) -> String {
    if let Some(name) = selector
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        return sanitize_component(name);
    }
    let named = match selector.id {
        Some(1) => "CURSOR",
        Some(2) => "BITMAP",
        Some(3) => "ICON",
        Some(4) => "MENU",
        Some(5) => "DIALOG",
        Some(6) => "STRING",
        Some(7) => "FONTDIR",
        Some(8) => "FONT",
        Some(9) => "ACCELERATOR",
        Some(10) => "RCDATA",
        Some(11) => "MESSAGETABLE",
        Some(12) => "GROUP_CURSOR",
        Some(14) => "GROUP_ICON",
        Some(16) => "VERSION",
        Some(17) => "DLGINCLUDE",
        Some(19) => "PLUGPLAY",
        Some(20) => "VXD",
        Some(21) => "ANICURSOR",
        Some(22) => "ANIICON",
        Some(23) => "HTML",
        Some(24) => "MANIFEST",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    match selector.id {
        Some(other) => format!("TYPE_{other}"),
        None => "UNKNOWN".to_string(),
    }
}

/// A resource's own name, as a file name (a numeric id keeps its digits).
fn resource_name(selector: &Selector) -> String {
    match (&selector.name, selector.id) {
        (Some(name), _) if !name.trim().is_empty() => name.trim().to_string(),
        (_, Some(id)) => id.to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Unpacking
// ---------------------------------------------------------------------------

/// Try to read a packed payload out of `candidates` and decode it.
///
/// The loader and its three members carry no labels of their own, so the
/// combination is searched: every candidate is tried as the loader and every
/// ordered triple as the members, and a combination is accepted only when the
/// whole chain — down to the SHA-1 at the end — agrees. That final check is
/// what makes the search safe: a wrong guess cannot produce a matching digest.
pub fn unpack(candidates: &[(String, Vec<u8>)]) -> Result<Decoded, Error> {
    if candidates.len() < BLOCKS + 1 {
        return Err(Error::NotAPackage);
    }
    // Computed once: the loader key is the concatenation of these.
    let hashes: Vec<String> = candidates.iter().map(|(_, d)| hex_upper(&HashAlg::Sha1.digest(d))).collect();

    for (li, (loader_name, loader)) in candidates.iter().enumerate() {
        if loader.len() < 32 || loader.len() % 16 != 0 {
            continue;
        }
        for a in 0..candidates.len() {
            if a == li {
                continue;
            }
            for b in 0..candidates.len() {
                if b == li || b == a {
                    continue;
                }
                for c in 0..candidates.len() {
                    if c == li || c == a || c == b {
                        continue;
                    }
                    let key_text = format!("{}{}{}", hashes[a], hashes[b], hashes[c]);
                    let key = derive(&CipherAlg::Aes256, key_text.as_bytes());
                    // One block is enough to reject almost everything.
                    if !ends_with_padding(&key, loader) {
                        continue;
                    }
                    let mut plain = CipherAlg::Aes256.apply(&key, &[0u8; 16], loader);
                    let Some(_keep) = strip_padding(&mut plain) else {
                        continue;
                    };
                    if plain.len() != PSW_DICT_LEN && !is_mostly_printable(&plain) {
                        continue;
                    }
                    let ids = [a, b, c];
                    let Some(hex) = member_hex(&plain, &ids, candidates) else {
                        continue;
                    };
                    // A wrong combination can still survive the cheap checks
                    // when a candidate is tiny, so a failure here means "keep
                    // looking" rather than "this file is broken".
                    let Ok(decoded) = decode(&plain, &hex) else {
                        continue;
                    };
                    let names = [&candidates[a].0, &candidates[b].0, &candidates[c].0];
                    return Ok(Decoded {
                        package: Package {
                            loader: loader_name.clone(),
                            members: [names[0].clone(), names[1].clone(), names[2].clone()],
                        },
                        text: decoded.0,
                        entries: decoded.1,
                    });
                }
            }
        }
    }
    Err(Error::NotAPackage)
}

/// Decrypt each member with the password `PswDict` holds for it.
///
/// `PswDict[i]` is the *length* of block `i`'s password, which is taken from
/// the front of the dictionary (wrapping if it runs off the end).
fn member_hex(
    psw_dict: &[u8],
    ids: &[usize; BLOCKS],
    candidates: &[(String, Vec<u8>)],
) -> Option<Vec<String>> {
    let mut out = Vec::with_capacity(BLOCKS);
    for (i, id) in ids.iter().enumerate() {
        let len = *psw_dict.get(i)? as usize;
        if len == 0 {
            return None;
        }
        let password = wrap_take(psw_dict, len);
        let key = derive(&CipherAlg::Rc4, &password);
        let plain = CipherAlg::Rc4.apply(&key, &[], &candidates[*id].1);
        if !plain.iter().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        // The RC4 output *is* the hex text — it is already the printable
        // representation, not bytes to be encoded again.
        out.push(String::from_utf8_lossy(&plain).to_ascii_uppercase());
    }
    Some(out)
}

/// Stages two to seven: split the hex blocks, decode the hash blob, recover
/// the passwords, decrypt the payload twice over, verify, and split.
fn decode(psw_dict: &[u8], members: &[String]) -> Result<(String, Vec<String>), String> {
    let dict_text: String = psw_dict.iter().map(|b| *b as char).collect();

    // ---- stage 2: strip `Data`, `Hash` and `Psw` off the end of each block.
    let mut data: [Option<Vec<u8>>; BLOCKS] = Default::default();
    let mut hashes: [Option<Vec<u8>>; BLOCKS] = Default::default();
    let mut psws: [Option<Vec<u8>>; BLOCKS] = Default::default();
    for i in 1..=BLOCKS {
        // `mix_blocks` is a plain cyclic shift, not a table: block `i`
        // takes its data from block `i`, its hash from `i+1`, its password
        // from `i+2`, wrapping.
        let d = (i - 1) % BLOCKS;
        let h = i % BLOCKS;
        let p = (i + 1) % BLOCKS;

        let text = &members[d];
        let (psw_hex, rest) = split_tail(text)?;
        psws[p] = Some(unhex(psw_hex).map_err(|e| e.to_string())?);
        let (hash_hex, rest) = split_tail(rest)?;
        hashes[h] = Some(decode_hash(hash_hex)?);
        data[d] = Some(unhex(rest).map_err(|e| e.to_string())?);
    }
    let mut data: [Vec<u8>; BLOCKS] = data.map(|d| d.unwrap_or_default());
    let hashes = hashes.map(|h| h.unwrap_or_default());
    let psws = psws.map(|p| p.unwrap_or_default());

    // ---- stages 3-5: per block, recover the passwords and decrypt.
    let mut parts = Vec::with_capacity(BLOCKS);
    for i in 0..BLOCKS {
        let spec_key = hash_password(&hashes[i], &dict_text)?;
        let key = derive(&CipherAlg::Aes192, spec_key.as_bytes());
        let mut spec = CipherAlg::Aes192.apply(&key, &[0u8; 16], &psws[i]);
        strip_padding(&mut spec).ok_or(tr("the AES-192 password block has no padding"))?;
        let spec = String::from_utf8_lossy(&spec).into_owned();
        let (first, second) = password_pair(&spec, &dict_text)?;

        let key = derive(&CipherAlg::Aes128, second.as_bytes());
        let mut plain = CipherAlg::Aes128.apply(&key, &[0u8; 16], &data[i]);
        strip_padding(&mut plain).ok_or(tr("the AES-128 layer has no padding"))?;
        let key = derive(&CipherAlg::Aes256, first.as_bytes());
        let mut plain = CipherAlg::Aes256.apply(&key, &[0u8; 16], &plain);
        strip_padding(&mut plain).ok_or(tr("the AES-256 layer has no padding"))?;

        // The format authenticates itself: the digest must match the one
        // carried alongside the block, which is why a wrong key cannot slip
        // through.
        if HashAlg::Sha1.digest(&plain) != hashes[i] {
            return Err(msg!("block {n} fails its SHA-1 check", n = i + 1));
        }
        parts.push(String::from_utf8_lossy(&plain).into_owned());
        data[i] = plain;
    }

    // ---- stages 6-7: concatenate, then split on the separator the format
    // stores in the first three bytes.
    let text: String = parts.concat();
    let mut chars = text.chars();
    let sep: String = chars.by_ref().take(3).collect();
    if sep.chars().count() < 3 {
        return Err(tr("the payload is too short to carry a separator").into());
    }
    let entries = chars.as_str().split(&sep).map(str::to_string).collect();
    Ok((text, entries))
}

/// Strip the trailing `len` from a block's hex text.
///
/// The last eight characters are a marker: its 1st, 3rd, 5th and 7th
/// characters spell a hex number, and that many characters sit just before the
/// marker. Returns `(the stripped run, what is left)`.
fn split_tail(text: &str) -> Result<(&str, &str), String> {
    if text.len() < 8 {
        return Err(tr("a block is too short to carry its length marker").into());
    }
    let tail = &text[text.len() - 8..];
    let marker: String = tail.chars().step_by(2).collect();
    let len = from_hex(&marker).ok_or(tr("a block's length marker is not hex"))?;
    let end = text.len() - 8;
    if len > end {
        return Err(tr("a block's length marker runs past its start").into());
    }
    Ok((&text[end - len..end], &text[..end - len]))
}

/// `decode_hash`: undo the arithmetic the hash blob was folded with.
fn decode_hash(text: &str) -> Result<Vec<u8>, String> {
    if text.len() < 4 {
        return Err(tr("a hash block is too short").into());
    }
    // Every other character is taken, and pairs of those form bytes.
    let chars: Vec<char> = text.chars().step_by(2).collect();
    let bytes: Vec<u8> = chars
        .chunks(2)
        .filter(|p| p.len() == 2)
        .map(|p| {
            let digits: String = p.iter().collect();
            u8::from_str_radix(&digits, 16).unwrap_or(0)
        })
        .collect();
    if bytes.len() < 2 {
        return Err(tr("a hash block decodes to nothing").into());
    }
    let mut folded: Vec<char> = bytes.iter().map(|b| *b as char).collect();

    // The last character is the seed each digit is shifted by, alternating
    // direction; the result is a string of hex digits again.
    let seed = dec1(*folded.last().unwrap()) as i32;
    let body_len = folded.len() - 1;
    let mut shifted = String::new();
    for (i, c) in folded.iter().take(body_len).enumerate() {
        let mut v = dec1(*c) as i32;
        if (i + 1) % 2 == 1 {
            v -= seed;
            if v < 0 {
                v += 16;
            }
        } else {
            v += seed;
            if v > 15 {
                v -= 16;
            }
        }
        shifted.push(std::char::from_digit(v as u32, 16).unwrap().to_ascii_uppercase());
    }
    folded = shifted.chars().collect();

    // The leading pair is how far to rotate; odd rotates right, even left.
    if folded.len() < 2 {
        return Err(tr("a hash block is too short to rotate").into());
    }
    let rotate = from_hex(&folded[..2].iter().collect::<String>()).unwrap_or(0);
    let mut body: Vec<char> = folded.split_off(2);
    let n = rotate.min(body.len());
    if rotate % 2 == 1 {
        let tail = body.split_off(body.len() - n);
        body.splice(0..0, tail);
    } else {
        let head: Vec<char> = body.drain(..n).collect();
        body.extend(head);
    }
    let joined: String = body.iter().collect();
    unhex(&joined).map_err(|e| e.to_string())
}

/// `hash_password`: read password fragments out of the dictionary.
///
/// The hash blob is a hex string read three characters at a time — two hex
/// digits of position, one of length — which index into `PswDict`.
fn hash_password(hash: &[u8], dict: &str) -> Result<String, String> {
    let text = hex_upper(hash);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    for group in chars.chunks(3) {
        if group.len() < 3 {
            break;
        }
        let pos = from_hex(&group[..2].iter().collect::<String>()).unwrap_or(0);
        let len = dec1(group[2]);
        if pos > 0 && len > 0 {
            out.push_str(&take_at(dict, pos as u32, len as u32));
        }
    }
    if out.is_empty() {
        return Err(tr("a hash block yields no password material").into());
    }
    Ok(out.chars().take(PASSWORD_MAX_LEN).collect())
}

/// `take_spec` plus the `"<n>.<m>,"` prefix that precedes it.
///
/// The AES-192 block decrypts to a small header and two comma-separated lists
/// of `length.position` pairs, which say which slices of the dictionary make
/// up the block's two passwords.
fn password_pair(spec: &str, dict: &str) -> Result<(String, String), String> {
    // `^(\d+)\.\d+,` — a decimal header, consumed before the lists.
    let digits: String = spec.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(tr("the AES-192 block has no length header").into());
    }
    let rest = &spec[digits.len()..];
    let rest = rest.strip_prefix('.').ok_or(tr("the password header is malformed"))?;
    let more: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &rest[more.len()..];
    let rest = rest.strip_prefix(',').ok_or(tr("the password header is malformed"))?;
    let n: usize = digits
        .parse()
        .map_err(|_| tr("the password header is not a number").to_string())?;

    // The two windows are `StringMid($s, 1, n)` and `StringMid($s, n + 2, n)`.
    // AutoIt's `StringMid` clamps rather than failing, so a short block simply
    // yields shorter lists — do not reject it here.
    let chars: Vec<char> = rest.chars().collect();
    let window = |start: usize| -> String { chars.iter().skip(start).take(n).collect() };
    Ok((take_spec(&window(0), dict), take_spec(&window(n + 1), dict)))
}

/// `take_spec`: join the dictionary slices a `length.position` list names.
fn take_spec(spec: &str, dict: &str) -> String {
    let mut out = String::new();
    for item in spec.split(',') {
        let mut parts = item.split('.');
        let (Some(len), Some(pos)) = (parts.next(), parts.next()) else {
            continue;
        };
        let len: usize = len.trim().parse().unwrap_or(0);
        let pos: usize = pos.trim().parse().unwrap_or(0);
        if len > 0 && pos > 0 {
            out.push_str(&take_at(dict, pos as u32, len as u32));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Small helpers, mirroring AutoIt's own semantics
// ---------------------------------------------------------------------------

/// `CryptDeriveKey(MD5(text))` for `cipher`.
fn derive(cipher: &CipherAlg, text: &[u8]) -> Vec<u8> {
    let digest = HashAlg::Md5.digest(text);
    cipher.derive_key(HashAlg::Md5, &digest).0
}

/// `AutoIt`'s `Dec` on a single character: the value of its leading hex digits,
/// or zero when there are none.
fn dec1(c: char) -> u32 {
    c.to_digit(16).unwrap_or(0)
}

/// AutoIt's `Dec` on a short hex string.
fn from_hex(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let mut v: usize = 0;
    for c in text.chars() {
        v = v.checked_mul(16)?.checked_add(c.to_digit(16)? as usize)?;
    }
    Some(v)
}

/// `StringMid($s, $pos, $len)` with a 1-based position, in characters.
fn take_at(s: &str, pos: u32, len: u32) -> String {
    s.chars().skip(pos.saturating_sub(1) as usize).take(len as usize).collect()
}

/// `wrap_take`: take `len` characters from the front of `dict`, wrapping
/// around the end rather than stopping.
fn wrap_take(dict: &[u8], len: usize) -> Vec<u8> {
    if dict.is_empty() {
        return Vec::new();
    }
    (0..len).map(|i| dict[i % dict.len()]).collect()
}

/// Upper-case hex, the shape `String($binary)` gives in AutoIt.
fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Inverse of [`hex_upper`], rejecting anything that is not hex.
fn unhex(text: &str) -> Result<Vec<u8>, Error> {
    if text.len() % 2 != 0 {
        return Err(Error::BadData(tr("a hex run has an odd length").into()));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|_| Error::BadData(tr("a hex run is not hexadecimal").into()))
        })
        .collect()
}

/// PKCS#7 length, if `data` ends with valid padding.
fn strip_padding(data: &mut Vec<u8>) -> Option<usize> {
    let n = *data.last()? as usize;
    if n == 0 || n > 16 || data.len() < n || !data[data.len() - n..].iter().all(|b| *b as usize == n) {
        return None;
    }
    let keep = data.len() - n;
    data.truncate(keep);
    Some(keep)
}

/// Whether a decryption of the final ciphertext block ends in valid padding.
///
/// Checking one block instead of all of them is what keeps the search for the
/// right combination cheap.
fn ends_with_padding(key: &[u8], ciphertext: &[u8]) -> bool {
    if ciphertext.len() < 32 {
        return false;
    }
    let split = ciphertext.len() - 32;
    let block = CipherAlg::Aes256.apply(key, &ciphertext[split..split + 16], &ciphertext[ciphertext.len() - 16..]);
    let n = block[15] as usize;
    n >= 1 && n <= 16 && block[16 - n..].iter().all(|b| *b as usize == n)
}

/// Whether the bytes are overwhelmingly printable ASCII — the decrypted
/// dictionary is an ASCII blob, so this rejects a wrong key that happened to
/// land on valid padding.
fn is_mostly_printable(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let printable = data.iter().filter(|b| (0x20..0x7f).contains(*b)).count();
    printable * 100 >= data.len() * 95
}

/// Look up payload entries by 1-based index.
///
/// The entries come out of the container in exactly the order a script's own
/// table uses them, `[0]` being the count and `[1]` the first entry — so an
/// index from a disassembly or a debugger session maps straight onto a line
/// here. `spec` is a comma-separated list of single indices and inclusive
/// ranges: `152`, `1-5,3148`.
///
/// This is the other half of the independent check the crate exists for:
/// decoding the payload by hand says what the table *should* hold, and this
/// says it without running the script that builds it.
pub fn select_entries(
    entries: &[String],
    spec: &str,
) -> Result<Vec<(usize, String)>, Error> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (from, to) = match part.split_once('-') {
            Some((a, b)) => (
                parse_index(a.trim())?,
                parse_index(b.trim())?,
            ),
            None => {
                let n = parse_index(part)?;
                (n, n)
            }
        };
        if from > to {
            return Err(Error::BadIndex(msg!(
                "{part} is a descending range",
                part = part
            )));
        }
        for n in from..=to {
            let entry = entries.get(n - 1).cloned().ok_or_else(|| {
                Error::BadIndex(msg!(
                    "{n} is past the end ({count} entries)",
                    n = n,
                    count = entries.len()
                ))
            })?;
            if !out.iter().any(|(seen, _)| *seen == n) {
                out.push((n, entry));
            }
        }
    }
    out.sort_by_key(|(n, _)| *n);
    Ok(out)
}

/// One 1-based index in a [`select_entries`] spec.
fn parse_index(text: &str) -> Result<usize, Error> {
    match text.parse::<usize>() {
        // The table itself is 1-based; `0` is the count, not an entry.
        Ok(n) if n >= 1 => Ok(n),
        _ => {
            let shown = format!("{text:?}");
            Err(Error::BadIndex(msg!(
                "{shown} is not a 1-based index",
                shown = shown
            )))
        }
    }
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../tests/unit/payload.rs"]
mod tests;
