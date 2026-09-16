//! `au3 unpack <PATH> [-o FILE] [--script] [--raw] [--table] [--at SPEC]`.
//!
//! The command covers the two things a build hides, and which one is meant is
//! the difference between two flags:
//!
//! * `--script` reads back the **compiled script** `aut2exe` embedded — the
//!   `.au3` source the program was built from, tokenised and compressed. It
//!   works on the `.exe` or on a chunk already dumped out of one, and finds
//!   the chunk by its `AU3!EA05`/`AU3!EA06` signature (or the `SCRIPT`
//!   resource), so no script is needed to ask for it.
//! * without `--script`, it decodes a **resource-packed payload**: an extra
//!   encrypted file set some builds keep in their `RT_RCDATA` resources. The
//!   resources can be handed over either way they exist in practice: as the
//!   files `AutoIt3Wrapper_Res_File_Add` staged next to the script (a
//!   directory of `__NAME`, `__Res64/NAME`, `__ResImage/_NAME`, or a plain
//!   dump), or still inside the `.exe` they were compiled into. Either way no
//!   AutoIt script is involved — the point is to read a build whose script is
//!   not at hand, which is exactly when the resource names cannot be looked
//!   up.

use std::path::Path;

use autoitv3_i18n::{msg, tr};
use autoitv3_unpack::script;
use autoitv3_unpack::{
    candidates_from_dir, candidates_from_image, resources_from_image, select_entries, unpack,
    write_resources, Layout,
};
use clap::Args;

use crate::args::{CliError, CliResult, OutputArgs};
use crate::output::write_output;

/// Arguments for `au3 unpack`.
#[derive(Args, Debug)]
pub struct UnpackArgs {
    /// A directory of resource files, a PE image, or a compiled-script chunk
    #[arg(value_name = "PATH")]
    pub input: String,

    /// Read the compiled script (`AU3!EA05`/`AU3!EA06`) instead of a
    /// resource-packed payload, and emit its `.au3` source
    #[arg(long, conflicts_with_all = ["raw", "table", "at"])]
    pub script: bool,

    /// Emit the concatenated payload instead of one entry per line
    #[arg(long)]
    pub raw: bool,

    /// Number the entries, so an index can be read straight off a
    /// disassembly or a debugger session
    #[arg(long)]
    pub table: bool,

    /// Only these entries: a 1-based index, or a comma-separated list with
    /// inclusive ranges (`152`, `1-5,3148`)
    #[arg(long, value_name = "SPEC")]
    pub at: Option<String>,

    /// Extract the build's resource files into this directory, one type per
    /// subdirectory (the default is <input>.unpacked next to the input)
    #[arg(long, value_name = "DIR", conflicts_with_all = ["script", "payload", "raw", "table", "at"])]
    pub dir: Option<String>,

    /// Decode the resource-packed payload and emit its entries, instead of
    /// extracting the resources
    #[arg(long, conflicts_with = "script")]
    pub payload: bool,

    /// Group the extracted files by resource type (RCDATA/NAME) instead of the
    /// staging layout AutoIt3Wrapper writes them in (__ResImage/_NAME)
    #[arg(long)]
    pub by_type: bool,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `unpack` subcommand.
pub fn run(args: &UnpackArgs) -> CliResult<()> {
    if args.script {
        return run_script(args);
    }
    // --raw/--table/--at only mean something for the packed payload, so asking
    // for one of them is asking for that payload rather than for the files.
    if args.payload || args.raw || args.table || args.at.is_some() {
        return run_payload(args);
    }
    let default_dir = default_extract_dir(&args.input);
    let dir = args.dir.as_deref().unwrap_or(default_dir.as_str());
    run_resources(args, Path::new(dir))
}

/// The directory the default extraction writes into: <input>.unpacked.
fn default_extract_dir(input: &str) -> String {
    let path = Path::new(input);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unpacked".to_string());
    path.with_file_name(format!("{stem}.unpacked"))
        .to_string_lossy()
        .into_owned()
}

/// `--dir`: write the resources the build carries out as files.
///
/// A build made with `#AutoIt3Wrapper_Res_File_Add` keeps each added file as an
/// `RT_RCDATA` resource named by that directive, so writing the image's
/// resources out under their own names hands those files back. The packed
/// payload some builds hide in the same resources is *not* what this does — for
/// that, run without `--dir`.
fn run_resources(args: &UnpackArgs, dir: &Path) -> CliResult<()> {
    let path = Path::new(&args.input);
    let resources: Vec<(String, String, Vec<u8>)> = if path.is_dir() {
        candidates_from_dir(path)
            .map_err(|e| CliError::failure(e.to_string()))?
            .into_iter()
            // A staged directory holds the files the wrapper added, and those
            // are RT_RCDATA unless the directive said otherwise: the layout
            // records no other type.
            .map(|(name, bytes)| ("RCDATA".to_string(), name, bytes))
            .collect()
    } else {
        resources_from_image(path).map_err(|e| CliError::failure(e.to_string()))?
    };
    if resources.is_empty() {
        return Err(CliError::failure(msg!(
            "{path}: no resources to look at",
            path = path.display()
        )));
    }
    let layout = if args.by_type { Layout::ByType } else { Layout::Stage };
    let written =
        write_resources(dir, &resources, layout).map_err(|e| CliError::failure(e.to_string()))?;
    for ((_, _, bytes), file) in resources.iter().zip(written.iter()) {
        let relative = file.strip_prefix(dir).unwrap_or(file).display().to_string();
        eprintln!(
            "{}",
            msg!(
                "  {name} -> {path} ({bytes} bytes)",
                name = relative,
                path = file.display(),
                bytes = bytes.len()
            )
        );
    }
    eprintln!(
        "{}",
        msg!(
            "{count} resource file(s) written to {dir}",
            count = written.len(),
            dir = dir.display()
        )
    );
    Ok(())
}

/// `--script`: locate the compiled script and print its source.
fn run_script(args: &UnpackArgs) -> CliResult<()> {
    let path = Path::new(&args.input);
    if path.is_dir() {
        return Err(CliError::failure(msg!(
            "{path} is a directory; --script reads a PE image or a compiled-script chunk",
            path = path.display()
        )));
    }
    let compiled = script::from_image(path).map_err(|e| CliError::failure(e.to_string()))?;
    let source = compiled.source().map_err(|e| CliError::failure(e.to_string()))?;

    // The container does not label which record is the script, only its
    // sub-type does — so saying what was found is part of the answer.
    eprintln!(
        "{}",
        msg!(
            "compiled script: {version} ({files} embedded file(s))",
            version = compiled.version,
            files = compiled.files.len()
        )
    );
    for file in &compiled.files {
        let sub_type = format!("{:?}", file.sub_type);
        let name = if file.name.is_empty() { tr("<unnamed>") } else { file.name.as_str() };
        eprintln!(
            "{}",
            msg!(
                "  {sub_type} {name} ({bytes} bytes)",
                sub_type = sub_type,
                name = name,
                bytes = file.data.len()
            )
        );
    }
    eprintln!("{}", msg!("  {bytes} bytes of source", bytes = source.len()));
    write_output(args.output.output.as_deref(), &source)
}

/// The default: decode a resource-packed payload.
fn run_payload(args: &UnpackArgs) -> CliResult<()> {
    let path = Path::new(&args.input);
    let candidates = if path.is_dir() {
        candidates_from_dir(path)
    } else {
        candidates_from_image(path)
    }
    .map_err(|e| CliError::failure(e.to_string()))?;

    if candidates.is_empty() {
        return Err(CliError::failure(msg!(
            "{path}: no resources to look at",
            path = path.display()
        )));
    }
    let decoded = unpack(&candidates).map_err(|e| CliError::failure(e.to_string()))?;

    eprintln!(
        "{}",
        msg!(
            "unpacked: loader {loader}, members {members0}/{members1}/{members2} ({resources} resources considered)",
            loader = decoded.package.loader,
            members0 = decoded.package.members[0],
            members1 = decoded.package.members[1],
            members2 = decoded.package.members[2],
            resources = candidates.len()
        )
    );
    eprintln!(
        "{}",
        msg!(
            "  {entries} entries, {bytes} bytes",
            entries = decoded.entries.len(),
            bytes = decoded.text.len()
        )
    );

    let body = if args.raw {
        decoded.text.clone()
    } else if let Some(spec) = &args.at {
        let picked = select_entries(&decoded.entries, spec).map_err(|e| CliError::failure(e.to_string()))?;
        let mut out = String::new();
        for (n, entry) in picked {
            out.push_str(&format!("{n}\t{entry}\n"));
        }
        out
    } else if args.table {
        let mut out = String::new();
        for (i, entry) in decoded.entries.iter().enumerate() {
            out.push_str(&format!("{}\t{entry}\n", i + 1));
        }
        out
    } else {
        let mut out = decoded.entries.join("\n");
        out.push('\n');
        out
    };
    write_output(args.output.output.as_deref(), &body)
}
