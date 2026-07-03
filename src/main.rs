use std::io;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Error;
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use globset::{Glob, GlobSetBuilder};

use there_can_only_be_one::dedupe::{
    Sample, ScanOptions, assemble_groups, bucket_by_size, chunk_hash, confirm_by_full_hash,
    take_empty_files,
};
use there_can_only_be_one::output;

#[derive(Parser)]
#[command(name = "tcobo", version, about = "Find duplicate files in a directory")]
struct Args {
    /// Directory to scan for duplicates.
    #[arg(required_unless_present_any = ["completions", "manpage"])]
    path: Option<PathBuf>,

    /// Emit results as JSON on stdout instead of the text listing.
    #[arg(long)]
    json: bool,

    /// Follow symbolic links while scanning.
    #[arg(long)]
    follow_symlinks: bool,

    /// Ignore files smaller than this many bytes.
    #[arg(long, value_name = "BYTES", default_value_t = 0)]
    min_size: u64,

    /// Exclude paths matching this glob (may be given multiple times).
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,

    /// Where to sample each file's bytes in the fast pre-hash pass. Middle/end
    /// can prune better for files with large fixed headers.
    #[arg(long, value_enum, default_value_t = Sample::Start)]
    sample: Sample,

    /// Write a full report (duplicate groups and skipped empty files) to a file.
    /// Given without a value, drops it at <PATH>/tcobo.log; pass an explicit
    /// path with `--log=<FILE>`.
    #[arg(long, value_name = "FILE", num_args = 0..=1, require_equals = true)]
    log: Option<Option<PathBuf>>,

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
        print_completions(shell);
        return Ok(());
    }
    if args.manpage {
        clap_mangen::Man::new(Args::command()).render(&mut io::stdout())?;
        return Ok(());
    }

    // Guaranteed present by `required_unless_present_any`.
    let path = args.path.as_deref().expect("path is required");
    let opts = scan_options(&args)?;

    // Progress and the summary go to stderr so the results on stdout (text or
    // JSON) stay clean and pipeable. This tool only reads and reports — it
    // never deletes.
    let started = Instant::now();
    eprintln!("Scanning {}...", path.display());
    let mut size_buckets = bucket_by_size(path, &opts);

    // Empty files are trivially identical, so set them aside up front rather
    // than hashing them and flooding the output with one giant group.
    let empties = take_empty_files(&mut size_buckets);
    let scanned = size_buckets.values().map(Vec::len).sum::<usize>() + empties.len();
    let candidates: usize = size_buckets
        .values()
        .filter(|v| v.len() > 1)
        .map(Vec::len)
        .sum();
    output::print_scan_start(scanned, candidates, empties.len());

    let groups = assemble_groups(confirm_by_full_hash(chunk_hash(size_buckets, opts.sample)));

    // `--log` with no value defaults to <scanned path>/tcobo.log.
    let log_path = match &args.log {
        Some(Some(file)) => Some(file.clone()),
        Some(None) => Some(path.join("tcobo.log")),
        None => None,
    };
    if let Some(log_path) = &log_path {
        output::write_log(
            log_path,
            scanned,
            candidates,
            started.elapsed(),
            &groups,
            &empties,
        )?;
    }

    if args.json {
        output::print_json(
            scanned,
            candidates,
            empties.len(),
            started.elapsed(),
            &groups,
        )?;
    } else {
        output::print_text(&groups);
    }

    output::print_summary(started.elapsed(), &groups);

    Ok(())
}

/// Build the scan options (symlink following, size floor, exclude globs, sample
/// strategy) from the parsed arguments.
fn scan_options(args: &Args) -> Result<ScanOptions, Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in &args.exclude {
        builder.add(Glob::new(pattern)?);
    }
    Ok(ScanOptions {
        follow_symlinks: args.follow_symlinks,
        min_size: args.min_size,
        exclude: builder.build()?,
        sample: args.sample,
    })
}

/// Write a shell completion script for `shell` to stdout.
fn print_completions(shell: Shell) {
    let mut cmd = Args::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut io::stdout());
}
