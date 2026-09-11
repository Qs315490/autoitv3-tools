//! `au3 unpack <PATH> [-o FILE] [--raw] [--table] [--at SPEC]` — decode a
//! resource-packed payload straight out of its resources.
//!
//! The resources can be handed over either way they exist in practice: as the
//! files `AutoIt3Wrapper_Res_File_Add` staged next to the script (a directory
//! of `__NAME`, `__Res64/NAME`, `__ResImage/_NAME`, or a plain dump), or still
//! inside the `.exe` they were compiled into. Either way no AutoIt script is
//! involved — the point of the command is to read a build whose script is not
//! at hand, which is exactly when the resource names cannot be looked up.

use std::path::Path;

use autoitv3_unpack::{candidates_from_dir, candidates_from_image, select_entries, unpack};
use clap::Args;

use crate::args::{CliError, CliResult, OutputArgs};
use crate::output::write_output;

/// Arguments for `au3 unpack`.
#[derive(Args, Debug)]
pub struct UnpackArgs {
    /// A directory of resource files, or a PE image holding them
    #[arg(value_name = "PATH")]
    pub input: String,

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
    let path = Path::new(&args.input);
    let candidates = if path.is_dir() {
        candidates_from_dir(path)
    } else {
        candidates_from_image(path)
    }
    .map_err(|e| CliError::failure(e.to_string()))?;

    if candidates.is_empty() {
        return Err(CliError::failure(format!(
            "{}: no resources to look at",
            path.display()
        )));
    }
    let decoded = unpack(&candidates).map_err(|e| CliError::failure(e.to_string()))?;

    eprintln!(
        "unpacked: loader {}, members {}/{}/{} ({} resources considered)",
        decoded.package.loader,
        decoded.package.members[0],
        decoded.package.members[1],
        decoded.package.members[2],
        candidates.len()
    );
    eprintln!("  {} entries, {} bytes", decoded.entries.len(), decoded.text.len());

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
