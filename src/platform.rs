//! Platform-specific filesystem metadata.
//!
//! Isolates the OS-dependent bits — the sampling block size and a physical-file
//! identity for hardlink dedup — so the rest of the engine stays portable.

use std::fs::Metadata;

/// 64 KiB ceiling on the amount of a file sampled during the chunk-hash pass.
/// Only used where a filesystem block size is available to cap (Unix).
#[cfg(unix)]
pub const MAX_CHUNK: u64 = 65_536;
/// Fallback chunk size when a filesystem reports no sensible block size, or on
/// platforms that don't expose one.
pub const DEFAULT_CHUNK: u64 = 4096;

/// A stable identity for a physical file, used to deduplicate hardlinks.
/// On Unix this is `(device, inode)`.
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

// Windows exposes file identity only through the still-unstable
// `windows_by_handle` feature (`Metadata::volume_serial_number`/`file_index`),
// so on stable we'd have to open each file by handle via the Win32 API to read
// it. Until that's worth the cost, non-Unix platforms skip hardlink dedup and
// keep every name (the `None` case in `bucket_by_size`).
//
// NOTE: we previously used those two methods here and it broke the Windows
// build (they require nightly). Revisit when `windows_by_handle` stabilizes
// (tracking issue: https://github.com/rust-lang/rust/issues/63010) — at that
// point this can return `Some((volume_serial_number, file_index))` and Windows
// gets hardlink dedup for free.
#[cfg(not(unix))]
pub fn physical_id(_meta: &Metadata) -> Option<PhysicalId> {
    None
}
