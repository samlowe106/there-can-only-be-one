use anyhow::Error;
use std::path::PathBuf;
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::Path,
};

use walkdir::WalkDir;

use std::os::unix::fs::MetadataExt;

use clap::Parser;

#[derive(clap::Parser)]
struct Args {
    path: PathBuf,
}

fn main() -> Result<(), Error> {
    let args = Args::parse();

    /*
    let path: PathBuf = std::env::args()
        .nth(1)
        .expect("usage: dedupe <path>")
        .into();
    */

    // bucket by size
    let mut size_buckets: HashMap<u64, Vec<_>> = HashMap::new();
    for entry in WalkDir::new(args.path) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let size = entry.metadata()?.len();
        size_buckets.entry(size).or_default().push(entry);
    }

    // chunk-hash files that share a size
    let mut buckets: HashMap<_, Vec<_>> = HashMap::new();
    let mut hasher = blake3::Hasher::new();
    for entries in size_buckets.values().filter(|v| v.len() > 1) {
        for entry in entries {
            let file_path = entry.path();
            let chunk_size = determine_block_size(file_path);
            let file = File::open(file_path)?;
            hasher.reset();
            let mut limited = file.take(chunk_size);
            io::copy(&mut limited, &mut hasher)?;
            buckets
                .entry(hasher.finalize())
                .or_insert_with(Vec::new)
                .push(entry);
        }
    }

    let mut duplicates: HashMap<_, Vec<_>> = HashMap::new();

    for entries in buckets.values().filter(|v| v.len() > 1) {
        for entry in entries {
            let mut file = File::open(entry.path())?;
            hasher.reset();
            io::copy(&mut file, &mut hasher)?; // no .take() — full file
            duplicates.entry(hasher.finalize()).or_default().push(entry);
        }
    }

    // duplicates now maps full-file hash -> Vec of paths that are truly identical
    for entries in duplicates.values().filter(|v| v.len() > 1) {
        println!("Duplicates:");
        for entry in entries {
            println!("  {}", entry.path().display());
        }
    }

    Ok(())
}

fn determine_block_size(path: &Path) -> u64 {
    const MAX_CHUNK: u64 = 65_536; // 64 KiB ceiling
    const DEFAULT: u64 = 4096;

    // feels weird to use a map on just one item
    std::fs::metadata(path)
        .map(|m| m.blksize().min(MAX_CHUNK))
        .unwrap_or(DEFAULT)
}
