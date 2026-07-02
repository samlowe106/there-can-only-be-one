//! Platform-specific filesystem metadata.
//!
//! Isolates the OS-dependent bits — the sampling block size and a physical-file
//! identity for hardlink dedup — so the rest of the engine stays portable.

use std::fs::Metadata;

/// 64 KiB ceiling on the amount of a file sampled during the chunk-hash pass.
pub const MAX_CHUNK: u64 = 65_536;
/// Fallback chunk size when a filesystem reports no sensible block size, or on
/// platforms that don't expose one.
pub const DEFAULT_CHUNK: u64 = 4096;

/// A stable identity for a physical file, used to deduplicate hardlinks.
/// On Unix this is `(device, inode)`; on Windows, `(volume serial, file index)`.
pub type PhysicalId = (u64, u64);

/// Number of leading bytes to sample when chunk-hashing this file.
#[cfg(unix)]
pub fn chunk_size(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    match meta.blksize() {
        0 => DEFAULT_CHUNK,
        blksize => blksize.min(MAX_CHUNK),
    }
}

#[cfg(not(unix))]
pub fn chunk_size(_meta: &Metadata) -> u64 {
    DEFAULT_CHUNK
}

/// The physical identity of this file, or `None` when the platform can't
/// provide one — in which case hardlinks simply can't be detected and each
/// name is treated as a distinct file.
#[cfg(unix)]
pub fn physical_id(meta: &Metadata) -> Option<PhysicalId> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(windows)]
pub fn physical_id(meta: &Metadata) -> Option<PhysicalId> {
    use std::os::windows::fs::MetadataExt;
    // Both are populated only when the metadata came from an open handle;
    // `walkdir` opens one, but fall back gracefully if they're missing.
    Some((u64::from(meta.volume_serial_number()?), meta.file_index()?))
}

#[cfg(not(any(unix, windows)))]
pub fn physical_id(_meta: &Metadata) -> Option<PhysicalId> {
    None
}
