//! Print a member of a Diablo II install: `mpq_cat <data dir> <member>`.
//!
//! For looking at the operator's own files while developing; prints nothing it was not asked
//! for, and the output is not meant to be committed.

use d2_formats::mpq::{ArchiveSet, DATA_ARCHIVES};
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, member] = args.as_slice() else {
        eprintln!("usage: mpq_cat <data dir> <member>");
        std::process::exit(2);
    };
    let set = match ArchiveSet::open(dir, &DATA_ARCHIVES) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    match set.read(member) {
        Ok(Some(bytes)) => {
            let _ = std::io::stdout().write_all(&bytes);
        }
        Ok(None) => {
            eprintln!("no such member");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
