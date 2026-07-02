//! Rendering scan results: text and JSON listings, progress lines, the empty
//! file log, and byte formatting.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::dedupe::{duplicate_files, reclaimable_bytes, DupGroup};

/// The JSON report emitted by [`print_json`].
#[derive(Serialize)]
struct Report {
    scanned: usize,
    candidates: usize,
    empty_files: usize,
    elapsed_ms: u128,
    reclaimable_bytes: u64,
    duplicate_files: usize,
    groups: Vec<Group>,
}

#[derive(Serialize)]
struct Group {
    size: u64,
    paths: Vec<String>,
}

/// Format a byte count with binary units (KiB/MiB/GiB/…), e.g. `1.47 GiB`.
pub fn human_readable_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B") // whole bytes need no decimals
    } else {
        format!("{size:.2} {}", UNITS[unit])
    }
}

/// Print the post-walk progress line to stderr, noting any skipped empties.
pub fn print_scan_start(scanned: usize, candidates: usize, empty_count: usize) {
    if empty_count > 0 {
        eprintln!(
            "Found {scanned} file(s), skipping {empty_count} empty; \
             {candidates} share a size and will be hashed."
        );
    } else {
        eprintln!("Found {scanned} file(s); {candidates} share a size and will be hashed.");
    }
}

/// Write the paths of skipped empty files to `path` (with a header), and note
/// on stderr where they went.
pub fn write_empty_log(path: &Path, empties: &[PathBuf]) -> io::Result<()> {
    let mut log = File::create(path)?;
    writeln!(
        log,
        "# {} empty (0-byte) file(s) — trivially identical, excluded from hashing.",
        empties.len()
    )?;
    for empty in empties {
        writeln!(log, "{}", empty.display())?;
    }
    if !empties.is_empty() {
        eprintln!("Wrote {} empty-file path(s) to {}.", empties.len(), path.display());
    }
    Ok(())
}

/// Print duplicate groups as a human-readable listing on stdout.
pub fn print_text(groups: &[DupGroup]) {
    for group in groups {
        println!("Duplicates ({} bytes each):", group.size);
        for path in &group.paths {
            println!("  {}", path.display());
        }
    }
}

/// Serialize the full report as pretty JSON on stdout.
pub fn print_json(
    scanned: usize,
    candidates: usize,
    empty_files: usize,
    elapsed: Duration,
    groups: &[DupGroup],
) -> serde_json::Result<()> {
    let report = Report {
        scanned,
        candidates,
        empty_files,
        elapsed_ms: elapsed.as_millis(),
        reclaimable_bytes: reclaimable_bytes(groups),
        duplicate_files: duplicate_files(groups),
        groups: groups
            .iter()
            .map(|g| Group {
                size: g.size,
                paths: g.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            })
            .collect(),
    };
    serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
    println!();
    Ok(())
}

/// Print the final summary line to stderr.
pub fn print_summary(elapsed: Duration, groups: &[DupGroup]) {
    eprintln!(
        "Done in {:.2?}: {} duplicate group(s) spanning {} file(s); {} reclaimable (nothing deleted).",
        elapsed,
        groups.len(),
        duplicate_files(groups),
        human_readable_bytes(reclaimable_bytes(groups)),
    );
}
