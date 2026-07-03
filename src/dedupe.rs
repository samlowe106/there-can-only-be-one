//! The duplicate-detection engine.
//!
//! Runs in three passes: bucket by size, chunk-hash same-size files to rule out
//! obvious mismatches cheaply, then fully hash the survivors to confirm.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use globset::GlobSet;
use rayon::prelude::*;
use walkdir::{DirEntry, WalkDir};

use crate::platform::{self, PhysicalId};

/// Key used to collapse hardlinks during the walk: files sharing a physical
/// identity dedup to one entry, while files with no available identity each get
/// a distinct key and are all kept.
#[derive(PartialEq, Eq, Hash)]
enum FileKey {
    Physical(PhysicalId),
    Distinct(usize),
}

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

impl FileInfo {
    /// The path to this file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consume this `FileInfo`, returning its path.
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

/// Where in each file the chunk-hash pass samples its bytes.
///
/// Sampling the start is cheapest (a sequential read), but files that share a
/// large fixed prefix — format headers, zero-padded or preallocated images —
/// collide there and fall through to the full-hash pass. Sampling the middle or
/// end skips such prefixes and can prune better, at the cost of a seek. The
/// choice never changes *which* duplicates are found, only how much data gets
/// fully hashed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Sample {
    /// The first bytes of the file (default).
    #[default]
    Start,
    /// A window centered in the file.
    Middle,
    /// The last bytes of the file.
    End,
}

/// Options controlling a scan.
pub struct ScanOptions {
    /// Follow symbolic links while walking (default: `false`).
    pub follow_symlinks: bool,
    /// Ignore files smaller than this many bytes (default: `0`).
    pub min_size: u64,
    /// Skip any path matching this glob set (default: matches nothing).
    pub exclude: GlobSet,
    /// Where the chunk-hash pass samples each file (default: [`Sample::Start`]).
    pub sample: Sample,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            min_size: 0,
            exclude: GlobSet::empty(),
            sample: Sample::Start,
        }
    }
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
pub fn find_duplicates(root: &Path, opts: &ScanOptions) -> Vec<DupGroup> {
    let size_buckets = bucket_by_size(root, opts);
    let chunk_buckets = chunk_hash(size_buckets, opts.sample);
    let by_hash = confirm_by_full_hash(chunk_buckets);
    assemble_groups(by_hash)
}

/// Remove and return the paths of empty (0-byte) files from `size_buckets`.
///
/// Empty files are trivially identical, so callers usually set them aside and
/// report them separately rather than hashing them into one giant group.
pub fn take_empty_files(size_buckets: &mut HashMap<u64, Vec<FileInfo>>) -> Vec<PathBuf> {
    size_buckets
        .remove(&0)
        .unwrap_or_default()
        .into_iter()
        .map(FileInfo::into_path)
        .collect()
}

/// Total bytes reclaimable by keeping a single copy of each duplicate group.
pub fn reclaimable_bytes(groups: &[DupGroup]) -> u64 {
    groups
        .iter()
        .map(|g| g.size * (g.paths.len() as u64 - 1))
        .sum()
}

/// Total number of files spanned by all duplicate groups.
pub fn duplicate_files(groups: &[DupGroup]) -> usize {
    groups.iter().map(|g| g.paths.len()).sum()
}

/// Pass 1: walk `root` and bucket files by size, honoring `opts` (symlink
/// following, minimum size, and exclude globs).
///
/// Hardlinks are deduplicated by collecting into a map keyed by physical
/// identity (see [`crate::platform`]): only one name survives per physical
/// file, so we never report two names for the same bytes (which waste no
/// disk). Walk errors and unreadable files are logged and skipped.
pub fn bucket_by_size(root: &Path, opts: &ScanOptions) -> HashMap<u64, Vec<FileInfo>> {
    WalkDir::new(root)
        .follow_links(opts.follow_symlinks)
        .into_iter()
        .filter_map(|entry| entry.map_err(|e| eprintln!("warning: {e}")).ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| !opts.exclude.is_match(entry.path()))
        .filter_map(file_info)
        .filter(|(_, info)| info.size >= opts.min_size)
        // Collecting into a map keyed by physical identity keeps one name per
        // physical file, dropping hardlinks. Files without an identity get a
        // distinct key so they're all retained.
        .enumerate()
        .map(|(i, (id, info))| (id.map_or(FileKey::Distinct(i), FileKey::Physical), info))
        .collect::<HashMap<FileKey, FileInfo>>()
        .into_values()
        .fold(HashMap::new(), |mut buckets, info| {
            buckets.entry(info.size).or_default().push(info);
            buckets
        })
}

/// Read the metadata for one walked file and build its [`FileInfo`], paired
/// with its physical identity (if the platform provides one) for hardlink
/// dedup. Returns `None` (after logging) if the file cannot be `stat`ed.
fn file_info(entry: DirEntry) -> Option<(Option<PhysicalId>, FileInfo)> {
    let meta = entry
        .metadata()
        .map_err(|e| eprintln!("warning: cannot stat {}: {e}", entry.path().display()))
        .ok()?;
    let id = platform::physical_id(&meta);
    let info = FileInfo {
        chunk_size: platform::chunk_size(&meta),
        size: meta.len(),
        path: entry.into_path(),
    };
    Some((id, info))
}

/// Pass 2: chunk-hash every file that shares its size with another, in
/// parallel, then group by (size, chunk-hash) — same size *and* same sampled
/// bytes (see [`Sample`]) are the only real duplicate candidates. If a file is
/// no larger than its chunk, this hash covers the whole file (the sample always
/// starts at offset 0 in that case), so pass 3 can reuse it.
pub fn chunk_hash(
    size_buckets: HashMap<u64, Vec<FileInfo>>,
    sample: Sample,
) -> HashMap<(u64, blake3::Hash), Vec<FileInfo>> {
    size_buckets
        .into_par_iter()
        .filter(|(_, v)| v.len() > 1)
        .flat_map(|(_, v)| v)
        .filter_map(|info| {
            let offset = sample_offset(sample, info.size, info.chunk_size);
            match hash_region(&info.path, offset, info.chunk_size) {
                Ok(hash) => Some((hash, info)),
                Err(e) => {
                    eprintln!("warning: cannot read {}: {e}", info.path.display());
                    None
                }
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
                hash_region(&info.path, 0, u64::MAX)
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
    groups.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.paths[0].cmp(&b.paths[0]))
    });
    groups
}

/// Hash `len` bytes starting at `offset` in `path` with BLAKE3. Pass
/// `len == u64::MAX` to hash through to the end of the file.
fn hash_region(path: &Path, offset: u64, len: u64) -> io::Result<blake3::Hash> {
    let mut file = File::open(path)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))?;
    }
    let mut hasher = blake3::Hasher::new();
    let mut reader = file.take(len);
    io::copy(&mut reader, &mut hasher)?;
    Ok(hasher.finalize())
}

/// The byte offset at which the chunk pass samples a file of `size`, given the
/// `chunk` sample length and the [`Sample`] strategy. When the file is no
/// larger than the chunk this is 0 for every strategy, so the sample covers the
/// whole file and equals its full hash.
fn sample_offset(sample: Sample, size: u64, chunk: u64) -> u64 {
    match sample {
        Sample::Start => 0,
        Sample::Middle => size.saturating_sub(chunk) / 2,
        Sample::End => size.saturating_sub(chunk),
    }
}
