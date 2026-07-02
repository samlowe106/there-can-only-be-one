use std::io;
use std::path::PathBuf;

use anyhow::Error;
use clap::{CommandFactory, Parser};
use clap_complete::Shell;

use there_can_only_be_one::dedupe::{
    assemble_groups, bucket_by_size, chunk_hash, confirm_by_full_hash,
};

#[derive(Parser)]
#[command(name = "tcobo", version, about = "Find duplicate files in a directory")]
struct Args {
    /// Directory to scan for duplicates.
    #[arg(required_unless_present_any = ["completions", "manpage"])]
    path: Option<PathBuf>,

    /// Generate a shell completion script to stdout and exit.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,

    /// Generate a man page (roff) to stdout and exit.
    #[arg(long)]
    manpage: bool,
}

fn main() -> Result<(), Error> {
    let args = Args::parse();

    if let Some(shell) = args.completions {
        let mut cmd = Args::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut io::stdout());
        return Ok(());
    }

    if args.manpage {
        clap_mangen::Man::new(Args::command()).render(&mut io::stdout())?;
        return Ok(());
    }

    // Guaranteed present by `required_unless_present_any`.
    let path = args.path.expect("path is required");

    // Progress goes to stderr so the duplicate listing on stdout stays clean
    // and pipeable. This tool only reads and reports — it never deletes.
    eprintln!("Scanning {}...", path.display());
    let size_buckets = bucket_by_size(&path);
    let scanned: usize = size_buckets.values().map(Vec::len).sum();
    let candidates: usize = size_buckets.values().filter(|v| v.len() > 1).map(Vec::len).sum();
    eprintln!("Found {scanned} file(s); {candidates} share a size and will be hashed.");

    let groups = assemble_groups(confirm_by_full_hash(chunk_hash(size_buckets)));

    let mut reclaimable = 0u64;
    for group in &groups {
        // Keeping one copy, the rest are reclaimable.
        reclaimable += group.size * (group.paths.len() as u64 - 1);
        println!("Duplicates ({} bytes each):", group.size);
        for path in &group.paths {
            println!("  {}", path.display());
        }
    }

    eprintln!(
        "Done: {} duplicate group(s); {reclaimable} bytes reclaimable (nothing deleted).",
        groups.len(),
    );

    Ok(())
}
