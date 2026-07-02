//! The duplicate-detection engine.
//!
//! Runs in three passes: bucket by size, chunk-hash same-size files to rule out
//! obvious mismatches cheaply, then fully hash the survivors to confirm.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use walkdir::{DirEntry, WalkDir};

/// 64 KiB ceiling on the amount of a file sampled during the chunk-hash pass.
const MAX_CHUNK: u64 = 65_536;
/// Fallback chunk size when a filesystem reports no sensible block size.
const DEFAULT_CHUNK: u64 = 4096;

/// A candidate file, carrying the metadata we gathered during the walk so we
/// never have to `stat` it again.
///
/// The pass functions expose this type in their signatures, but its fields are
/// private: callers thread values through the pipeline without inspecting them.
pub struct FileInfo {
    path: PathBuf,
    size: u64,
    chunk_size: u64,
}

/// A group of files that are byte-for-byte identical.
pub struct DupGroup {
    /// The size, in bytes, of every file in the group.
    pub size: u64,
    /// The paths of the identical files, sorted.
    pub paths: Vec<PathBuf>,
}

/// Walk `root` and return groups of files with identical contents.
///
/// Unreadable files and directory-walk errors are reported to stderr and
/// skipped rather than aborting the whole scan.
pub fn find_duplicates(root: &Path) -> Vec<DupGroup> {
    let size_buckets = bucket_by_size(root);
    let chunk_buckets = chunk_hash(size_buckets);
    let by_hash = confirm_by_full_hash(chunk_buckets);
    assemble_groups(by_hash)
}

/// Pass 1: walk `root` and bucket files by size.
///
/// Hardlinks are deduplicated by collecting into a map keyed by (device,
/// inode): only one name survives per physical file, so we never report two
/// names for the same bytes (which waste no disk). Walk errors and unreadable
/// files are logged and skipped.
pub fn bucket_by_size(root: &Path) -> HashMap<u64, Vec<FileInfo>> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.map_err(|e| eprintln!("warning: {e}")).ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(file_info)
        // Collecting into a map keyed by (device, inode) keeps one name per
        // physical file, dropping hardlinks.
        .collect::<HashMap<(u64, u64), FileInfo>>()
        .into_values()
        .fold(HashMap::new(), |mut buckets, info| {
            buckets.entry(info.size).or_default().push(info);
            buckets
        })
}

/// Read the metadata for one walked file and build its [`FileInfo`], keyed by
/// its (device, inode) so callers can deduplicate hardlinks. Returns `None`
/// (after logging) if the file cannot be `stat`ed.
fn file_info(entry: DirEntry) -> Option<((u64, u64), FileInfo)> {
    let meta = entry
        .metadata()
        .map_err(|e| eprintln!("warning: cannot stat {}: {e}", entry.path().display()))
        .ok()?;
    let chunk_size = if meta.blksize() == 0 {
        DEFAULT_CHUNK
    } else {
        meta.blksize().min(MAX_CHUNK)
    };
    let info = FileInfo {
        path: entry.into_path(),
        size: meta.len(),
        chunk_size,
    };
    Some(((meta.dev(), meta.ino()), info))
}

/// Pass 2: chunk-hash every file that shares its size with another, in
/// parallel, then group by (size, chunk-hash) — same size *and* same leading
/// bytes are the only real duplicate candidates. If a file is no larger than
/// its chunk, this hash already covers the whole file, so pass 3 can reuse it.
pub fn chunk_hash(
    size_buckets: HashMap<u64, Vec<FileInfo>>,
) -> HashMap<(u64, blake3::Hash), Vec<FileInfo>> {
    size_buckets
        .into_par_iter()
        .filter(|(_, v)| v.len() > 1)
        .flat_map(|(_, v)| v)
        .filter_map(|info| match hash_file(&info.path, info.chunk_size) {
            Ok(hash) => Some((hash, info)),
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}", info.path.display());
                None
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .fold(HashMap::new(), |mut buckets, (hash, info)| {
            buckets.entry((info.size, hash)).or_default().push(info);
            buckets
        })
}

/// Pass 3: fully hash the survivors to confirm, grouping by full hash. Files
/// already covered by the chunk pass reuse that hash instead of being read a
/// second time.
pub fn confirm_by_full_hash(
    chunk_buckets: HashMap<(u64, blake3::Hash), Vec<FileInfo>>,
) -> HashMap<blake3::Hash, Vec<FileInfo>> {
    chunk_buckets
        .into_par_iter()
        .filter(|(_, v)| v.len() > 1)
        .flat_map(|((_, chunk_hash), v)| v.into_par_iter().map(move |info| (chunk_hash, info)))
        .filter_map(|(chunk_hash, info)| {
            let full = if info.size <= info.chunk_size {
                Some(chunk_hash) // the chunk pass already read the whole file
            } else {
                hash_file(&info.path, u64::MAX)
                    .map_err(|e| eprintln!("warning: cannot read {}: {e}", info.path.display()))
                    .ok()
            };
            full.map(|hash| (hash, info))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .fold(HashMap::new(), |mut acc, (hash, info)| {
            acc.entry(hash).or_default().push(info);
            acc
        })
}

/// Turn hash buckets into the reported duplicate groups: drop singletons, sort
/// paths within each group, then order the groups deterministically — largest
/// first, then by path.
pub fn assemble_groups(by_hash: HashMap<blake3::Hash, Vec<FileInfo>>) -> Vec<DupGroup> {
    let mut groups: Vec<DupGroup> = by_hash
        .into_values()
        .filter(|v| v.len() > 1)
        .map(|mut v| {
            v.sort_by(|a, b| a.path.cmp(&b.path));
            DupGroup {
                size: v[0].size,
                paths: v.into_iter().map(|f| f.path).collect(),
            }
        })
        .collect();
    groups.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.paths[0].cmp(&b.paths[0])));
    groups
}

/// Hash up to `limit` bytes from the start of `path` with BLAKE3.
/// Pass `u64::MAX` to hash the entire file.
fn hash_file(path: &Path, limit: u64) -> io::Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    let mut reader = File::open(path)?.take(limit);
    io::copy(&mut reader, &mut hasher)?;
    Ok(hasher.finalize())
}
