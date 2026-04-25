//! `parity` binary — diff two SQLite databases produced by Python and Rust ingest.
//!
//! Usage:
//!   parity <python-db> <rust-db>
//!
//! Exits with code 0 if the databases are equivalent, 1 if they diverge.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let py = PathBuf::from(args.next().ok_or_else(|| {
        anyhow::anyhow!("Usage: parity <python-db> <rust-db>\narg 1: python DB path is required")
    })?);
    let rs = PathBuf::from(args.next().ok_or_else(|| {
        anyhow::anyhow!("Usage: parity <python-db> <rust-db>\narg 2: rust DB path is required")
    })?);

    let report = hooked::parity::diff_databases(&py, &rs)?;
    print!("{}", report.summary());

    if !report.is_ok() {
        std::process::exit(1);
    }

    Ok(())
}
