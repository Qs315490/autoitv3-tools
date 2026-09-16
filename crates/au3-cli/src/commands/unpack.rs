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
use autoitv3_unpack::{candidates_from_dir, candidates_from_image, select_entries, unpack};
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

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `unpack` subcommand.
pub fn run(args: &UnpackArgs) -> CliResult<()> {
    if args.script {
        return run_script(args);
    }
    run_payload(args)
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
