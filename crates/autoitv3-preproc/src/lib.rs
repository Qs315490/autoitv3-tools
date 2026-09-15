//! `#include` expansion for AutoIt v3 scripts.
//!
//! AutoIt's preprocessor inserts the contents of an included file at the point
//! of the `#include` directive, so a script's constants and helper functions
//! come from the files it names. This crate does the same to the *parsed*
//! program: each directive is replaced by the items of the file it names,
//! recursively. Concatenating the text would have the same effect and would
//! throw the script's own line numbers away, which every diagnostic and the
//! debugger's breakpoints use; splicing items keeps them.
//!
//! # Search order
//!
//! The help page's two tables, in order:
//!
//! * `#include <file>` — the standard library (the running interpreter's own
//!   `Include` directory, then the usual AutoIt installs), then the user-defined
//!   libraries, then the script's own directory;
//! * `#include "file"` — the script's directory, then the user-defined libraries
//!   in reverse, then the standard library.
//!
//! The user-defined libraries are the `-I` directories and `AU3_INCLUDE_PATH`,
//! which stand in for the registry value the help page names
//! (`HKEY_CURRENT_USER\Software\AutoIt v3\AutoIt\Include`).
//!
//! # What it does not do
//!
//! A file that cannot be found is a *warning*, not an error: an analysis run
//! still has everything the script itself says, and AutoIt would not get that
//! far. A file that cannot be parsed *is* an error, because nothing can be known
//! about the script that included it. A compiled include (`.a3x`) is reported as
//! a warning — reading one back needs the unpacker, which this layer does not
//! depend on.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use autoitv3_ast::ast::{Item, ItemKind};
use autoitv3_ast::{parse, Program};

/// How deeply includes may nest before the expansion gives up.
///
/// AutoIt has its own limit; this one is only here so a pathological script
/// cannot make the tool recurse for ever.
const MAX_DEPTH: usize = 64;

/// Where `#include` looks for a file.
#[derive(Debug, Clone, Default)]
pub struct Includes {
    /// The standard library: `Include` directories an AutoIt install ships.
    standard: Vec<PathBuf>,
    /// The user-defined libraries: `-I` directories and `AU3_INCLUDE_PATH`.
    user: Vec<PathBuf>,
}

impl Includes {
    /// No search path at all: only what a directive names relative to the
    /// including file resolves.
    pub fn new() -> Self {
        Self::default()
    }

    /// The standard library as this machine has it.
    ///
    /// The help page says "the path of the currently running interpreter with
    /// `\Include` appended". This tool *is* its own interpreter and has no
    /// `Include` directory next to the binary, so the well-known AutoIt install
    /// directories are searched as well — on a host without one the list is
    /// empty, and `-I`/`AU3_INCLUDE_PATH` are what a script then needs.
    pub fn standard() -> Self {
        let mut includes = Self::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                includes.push_standard(dir.join("Include"));
            }
        }
        for var in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432"] {
            if let Ok(base) = std::env::var(var) {
                includes.push_standard(Path::new(&base).join("AutoIt3").join("Include"));
            }
        }
        includes
    }

    /// The standard library plus the user libraries named by the environment.
    pub fn from_env() -> Self {
        let mut includes = Self::standard();
        for var in ["AU3_INCLUDE_PATH", "AUTOIT_INCLUDE_PATH"] {
            let Ok(value) = std::env::var(var) else {
                continue;
            };
            for dir in split_paths(&value) {
                includes.push_user_dir(dir);
            }
        }
        includes
    }

    /// Add one standard-library directory.
    pub fn with_standard_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.push_standard(dir.into());
        self
    }

    /// Add one user-defined library.
    pub fn with_user_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.push_user_dir(dir.into());
        self
    }

    pub fn push_standard(&mut self, dir: PathBuf) {
        if dir.is_dir() && !self.standard.contains(&dir) {
            self.standard.push(dir);
        }
    }

    pub fn push_user_dir(&mut self, dir: PathBuf) {
        if !self.user.contains(&dir) {
            self.user.push(dir);
        }
    }

    pub fn standard_dirs(&self) -> &[PathBuf] {
        &self.standard
    }

    pub fn user_dirs(&self) -> &[PathBuf] {
        &self.user
    }

    /// The directories one directive searches, in order.
    ///
    /// `script_dir` is the directory of the file the directive is in, which for
    /// a nested include is that file's own directory, not the top-level script's.
    pub fn search_order(&self, quoted: bool, script_dir: &Path) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut push = |dir: PathBuf| {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        };
        if quoted {
            push(script_dir.to_path_buf());
            for dir in self.user.iter().rev() {
                push(dir.clone());
            }
            for dir in &self.standard {
                push(dir.clone());
            }
        } else {
            for dir in &self.standard {
                push(dir.clone());
            }
            for dir in &self.user {
                push(dir.clone());
            }
            push(script_dir.to_path_buf());
        }
        dirs
    }

    /// The first file `target` names, when there is one.
    pub fn resolve(&self, target: &str, quoted: bool, script_dir: &Path) -> Option<PathBuf> {
        let direct = Path::new(target);
        if direct.is_absolute() {
            return direct.is_file().then(|| direct.to_path_buf());
        }
        self.search_order(quoted, script_dir)
            .into_iter()
            .map(|dir| dir.join(target))
            .find(|candidate| candidate.is_file())
    }
}

/// A program with every `#include` it named expanded in place.
pub struct Expansion {
    /// The script's items, with each include's items spliced in where the
    /// directive was.
    pub program: Program,
    /// Every file that was read, the script itself first.
    pub files: Vec<PathBuf>,
    /// What could not be done, in the order it was noticed.
    pub warnings: Vec<String>,
}

/// Something that stopped the expansion.
#[derive(Debug, Clone)]
pub struct Error {
    message: String,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Expand every `#include` of `program`, which was parsed from `path`.
pub fn expand(program: Program, path: &Path, includes: &Includes) -> Result<Expansion, Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut state = State {
        includes,
        once: HashSet::new(),
        stack: Vec::new(),
        files: vec![path.to_path_buf()],
        warnings: Vec::new(),
    };
    let items = state.expand_items(program.items, dir, 0)?;
    Ok(Expansion {
        program: Program {
            items,
            comments: program.comments,
        },
        files: state.files,
        warnings: state.warnings,
    })
}

/// What one file being expanded remembers.
struct State<'a> {
    includes: &'a Includes,
    /// Files whose `#include-once` was seen, keyed by canonical path.
    once: HashSet<PathBuf>,
    /// The files currently being expanded, so a cycle is noticed instead of
    /// recursing for ever.
    stack: Vec<PathBuf>,
    files: Vec<PathBuf>,
    warnings: Vec<String>,
}

impl State<'_> {
    fn expand_items(
        &mut self,
        items: Vec<Item>,
        dir: &Path,
        depth: usize,
    ) -> Result<Vec<Item>, Error> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            match &item.kind {
                ItemKind::Directive(text) => {
                    let (name, argument) = split_directive(text);
                    if name.eq_ignore_ascii_case("include-once") {
                        // Only the file it stands in has anything to do with it,
                        // and that was read before it got here.
                        continue;
                    }
                    if name.eq_ignore_ascii_case("include") {
                        if let Some(mut included) = self.include(argument, dir, depth)? {
                            out.append(&mut included);
                        }
                        continue;
                    }
                    out.push(item);
                }
                // A `#region` is a wrapper around items, so a directive inside
                // one is expanded like any other.
                ItemKind::Region(region) => {
                    let mut region = region.clone();
                    region.items = self.expand_items(region.items, dir, depth)?;
                    out.push(Item {
                        kind: ItemKind::Region(region),
                        span: item.span,
                    });
                }
                _ => out.push(item),
            }
        }
        Ok(out)
    }

    /// The items of the file `argument` names, or `None` when there are none to
    /// splice (`Ok(None)`) or the script is broken (`Err`).
    fn include(
        &mut self,
        argument: &str,
        dir: &Path,
        depth: usize,
    ) -> Result<Option<Vec<Item>>, Error> {
        let Some((target, quoted)) = include_argument(argument) else {
            self.warnings.push(format!(
                "#include without a file name ({argument}) — the directive was ignored"
            ));
            return Ok(None);
        };
        let Some(path) = self.includes.resolve(&target, quoted, dir) else {
            let searched = self
                .includes
                .search_order(quoted, dir)
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.warnings.push(format!(
                "#include {argument} not found (searched {searched}) — \
                 put the AutoIt Include directory in AU3_INCLUDE_PATH or pass -I DIR"
            ));
            return Ok(None);
        };
        let key = canonical(&path);
        if self.once.contains(&key) {
            return Ok(None);
        }
        if self.stack.contains(&key) {
            self.warnings.push(format!(
                "#include {} is already being expanded (cyclic include) — it was skipped",
                path.display()
            ));
            return Ok(None);
        }
        if depth >= MAX_DEPTH {
            return Err(Error::new(format!(
                "#include {} nests more than {MAX_DEPTH} levels deep",
                path.display()
            )));
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
        if is_compiled(&bytes) {
            self.warnings.push(format!(
                "#include {} is a compiled file (.a3x), which is not expanded",
                path.display()
            ));
            return Ok(None);
        }
        // An include file may be ANSI, which AutoIt accepts: anything that is
        // not valid UTF-8/UTF-16 is read as if it were Latin-1 rather than
        // dropped, so the ASCII parts still define what the script asked for.
        let source = decode(&bytes).unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
        let program = parse(&source)
            .map_err(|e| Error::new(format!("parse error in {}: {e}", path.display())))?;
        if has_include_once(&program) && !self.once.insert(key.clone()) {
            return Ok(None);
        }
        let nested = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        self.files.push(path.clone());
        self.stack.push(key);
        let items = self.expand_items(program.items, &nested, depth + 1)?;
        self.stack.pop();
        Ok(Some(items))
    }
}

/// The file name and form of an `#include`'s argument.
///
/// `"file"` is relative to the including script, `<file>` to the standard
/// library; anything else — including the `;` comment the lexer kept — is not a
/// file name at all.
fn include_argument(argument: &str) -> Option<(String, bool)> {
    let rest = argument.trim();
    let (open, close) = match rest.as_bytes().first()? {
        b'"' => ('"', '"'),
        b'<' => ('<', '>'),
        _ => return None,
    };
    let _ = open;
    let end = rest[1..].find(close)? + 1;
    Some((rest[1..end].to_string(), close == '"'))
}

/// A directive line as `(name, argument)`: `include <x.au3>` → `("include",
/// "<x.au3>")`, `include-once` → `("include-once", "")`.
fn split_directive(text: &str) -> (&str, &str) {
    let text = text.trim();
    match text.find(char::is_whitespace) {
        Some(at) => (&text[..at], text[at..].trim_start()),
        None => (text, ""),
    }
}

/// Whether the bytes are a compiled script rather than source text.
fn is_compiled(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ") || bytes.starts_with(b"AU3!EA")
}

/// Whether a file says `#include-once`.
fn has_include_once(program: &Program) -> bool {
    program.items.iter().any(|item| match &item.kind {
        ItemKind::Directive(text) => split_directive(text)
            .0
            .eq_ignore_ascii_case("include-once"),
        _ => false,
    })
}

/// A path as the file system spells it, so the same file named two ways is one
/// entry in the `#include-once` set.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Split a `AU3_INCLUDE_PATH` value into directories.
///
/// The registry value the help page names is `;`-delimited, and a Windows path
/// contains a `:`, so `;` wins when it is there; otherwise the platform's own
/// separator is used, which is what makes `-I a:b` work on a Unix host.
fn split_paths(value: &str) -> Vec<PathBuf> {
    if value.contains(';') {
        return value
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    std::env::split_paths(value).collect()
}


/// Decode AutoIt source bytes, when their encoding can be told apart from a
/// binary file's.
///
/// UTF-8 (with or without a BOM) and UTF-16 with a BOM are the encodings AutoIt
/// itself accepts; `None` means the bytes are neither, which for a `.au3` file
/// means it is not a script.
pub fn decode(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(String::from_utf8_lossy(rest).into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Some(decode_utf16(rest, true));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Some(decode_utf16(rest, false));
    }
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    // `chunks(2)` so a trailing odd byte is dropped, as AutoIt does.
    let units: Vec<u16> = bytes
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoitv3_ast::ast::{Stmt, StmtKind};

    /// A fresh directory for one test, under the system temp dir.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("au3-preproc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn expand_file(path: &Path) -> Expansion {
        let source = std::fs::read_to_string(path).unwrap();
        let program = parse(&source).unwrap();
        expand(program, path, &Includes::new()).unwrap()
    }

    /// The names of the top-level `Global Const` declarations, in order.
    fn consts(expansion: &Expansion) -> Vec<String> {
        expansion
            .program
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Stmt(Stmt { kind: StmtKind::VarDecl(decl), .. }) => {
                    Some(decl.vars.iter().map(|v| v.name.name.clone()))
                }
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn a_quoted_include_is_spliced_in_place() {
        let dir = scratch("quoted");
        write(&dir, "inner.au3", "Global Const $B = 2\n");
        write(
            &dir,
            "consts.au3",
            "Global Const $A = 1\n#include \"inner.au3\"\n",
        );
        let main = write(
            &dir,
            "main.au3",
            "#include \"consts.au3\"\nGlobal Const $C = 3\n",
        );
        let expansion = expand_file(&main);
        assert_eq!(consts(&expansion), ["$A", "$B", "$C"]);
        assert!(
            !expansion
                .program
                .items
                .iter()
                .any(|item| matches!(item.kind, ItemKind::Directive(_))),
            "the directives are not items any more"
        );
        assert_eq!(expansion.files.len(), 3, "{:?}", expansion.files);
        assert!(expansion.warnings.is_empty(), "{:?}", expansion.warnings);
    }

    #[test]
    fn include_once_keeps_the_second_include_out() {
        let dir = scratch("once");
        write(&dir, "helper.au3", "#include-once\nGlobal Const $H = 1\n");
        let main = write(
            &dir,
            "main.au3",
            "#include \"helper.au3\"\n#include \"helper.au3\"\n",
        );
        let expansion = expand_file(&main);
        assert_eq!(consts(&expansion), ["$H"]);
    }

    #[test]
    fn an_angled_include_searches_the_standard_library() {
        let dir = scratch("angled");
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        write(&library, "Std.au3", "Global Const $S = 1\n");
        write(&dir, "Std.au3", "Global Const $LOCAL = 1\n");
        let main = write(&dir, "main.au3", "#include <Std.au3>\nGlobal Const $C = 2\n");
        let includes = Includes::new().with_standard_dir(&library);
        let program = parse(&std::fs::read_to_string(&main).unwrap()).unwrap();
        let expansion = expand(program, &main, &includes).unwrap();
        // `<...>` does not look in the script's directory, so the library copy
        // is the one that was read.
        assert_eq!(consts(&expansion), ["$S", "$C"]);
    }

    #[test]
    fn a_missing_include_is_a_warning() {
        let dir = scratch("missing");
        let main = write(&dir, "main.au3", "#include \"nope.au3\"\nGlobal Const $C = 1\n");
        let expansion = expand_file(&main);
        assert_eq!(consts(&expansion), ["$C"]);
        assert_eq!(expansion.warnings.len(), 1, "{:?}", expansion.warnings);
        assert!(expansion.warnings[0].contains("nope.au3"));
    }

    #[test]
    fn a_cycle_is_broken_with_a_warning() {
        let dir = scratch("cycle");
        write(&dir, "a.au3", "#include \"b.au3\"\n");
        write(&dir, "b.au3", "#include \"a.au3\"\n");
        let main = write(&dir, "main.au3", "#include \"a.au3\"\nGlobal Const $C = 1\n");
        let expansion = expand_file(&main);
        assert_eq!(consts(&expansion), ["$C"]);
        assert!(
            expansion.warnings.iter().any(|w| w.contains("cyclic")),
            "{:?}",
            expansion.warnings
        );
    }

    #[test]
    fn a_utf16_include_is_read() {
        let dir = scratch("utf16");
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for unit in "Global Const $W = 1\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(dir.join("wide.au3"), bytes).unwrap();
        let main = write(&dir, "main.au3", "#include \"wide.au3\"\n");
        let expansion = expand_file(&main);
        assert_eq!(consts(&expansion), ["$W"]);
    }

    #[test]
    fn decode_tells_source_from_binary() {
        assert_eq!(decode(b"abc").as_deref(), Some("abc"));
        assert_eq!(decode(b"\xEF\xBB\xBFabc").as_deref(), Some("abc"));
        assert_eq!(decode(&[0xFF, 0xFE, b'a', 0]).as_deref(), Some("a"));
        assert_eq!(decode(&[0xFE, 0xFF, 0, b'a']).as_deref(), Some("a"));
        assert!(decode(&[0x00, 0x9F, 0xFF]).is_none());
    }
}

#[cfg(test)]
mod compiled_tests {
    use super::*;

    #[test]
    fn a_compiled_include_is_a_warning() {
        let dir = std::env::temp_dir().join(format!("au3-preproc-a3x-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.a3x"), b"MZ\0\0not really a build").unwrap();
        let main = dir.join("main.au3");
        std::fs::write(&main, "#include \"lib.a3x\"\nGlobal Const $C = 1\n").unwrap();
        let program = parse(&std::fs::read_to_string(&main).unwrap()).unwrap();
        let expansion = expand(program, &main, &Includes::new()).unwrap();
        assert!(
            expansion.warnings.iter().any(|w| w.contains("compiled")),
            "{:?}",
            expansion.warnings
        );
        assert_eq!(expansion.program.items.len(), 1);
    }
}
