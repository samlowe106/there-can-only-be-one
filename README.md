# There Can Only Be One (tcobo)

This is a program which finds duplicated files in a specified path. Ran on a 60GB folder with ~25,000 files in under 2 seconds, identifying over 800 true duplicate files.

Usage: `tcobo <path>`

Results (the duplicate groups) are written to **stdout**; progress, warnings, and the final summary go to **stderr**, so you can redirect the results cleanly (`tcobo ~/Pictures > dupes.txt`). The tool is strictly read-only — it never deletes or modifies files.

### Options

```
--json                 Emit results as JSON on stdout instead of the text listing
--follow-symlinks      Follow symbolic links while scanning (default: off)
--min-size <BYTES>     Ignore files smaller than this many bytes
--exclude <GLOB>       Exclude paths matching this glob (may be given multiple times)
```

For example, to scan for duplicate photos of at least 1 MiB, skipping any cache directories, and print JSON:

```sh
tcobo ~/Pictures --min-size 1048576 --exclude '**/cache/**' --json
```

### Generating shell completions and a man page

```sh
tcobo --completions bash > tcobo.bash   # also: zsh, fish, powershell, elvish
tcobo --manpage > tcobo.1
```

## Optimizations

Duplicate checking happens in multiple passes. First, all files in the directory are bucketed by file size, then a small amount of the file (based on the disk's block size) is sampled from the start of the file and hashed using the [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) hashing algorithm. This quickly and inexpensively identifies true negatives that can be ruled out as duplicates.

From there, any hash collisions are inspected by fully hashing all colliding files. Any files whose hashes collide are reported as a group as duplicates. Files small enough to have been read in full during the sampling pass reuse that hash instead of being read again.

Hashing runs in parallel across files ([rayon](https://github.com/rayon-rs/rayon)). Hardlinks are deduplicated by `(device, inode)` so multiple names for the same physical file are not reported as duplicates. Unreadable files and directory-walk errors are logged to stderr and skipped rather than aborting the scan.

## TODOs

1. **Opt-in delete mode.** Add an action to reclaim space (delete redundant copies, or replace them with hard/symlinks), with report-only remaining the default and deletion gated behind an explicit flag plus a keep-policy for which copy survives. (This subsumes the old "dry run mode" idea — the tool is already effectively a dry run today.)
2. **Cross-platform support.** The scan currently relies on `std::os::unix::fs::MetadataExt`, so it is Unix-only. The block-size sampling just needs a non-Unix fallback, but the bigger piece is the hardlink dedup, which keys on `(device, inode)` — Windows needs file IDs (`GetFileInformationByHandle`) or to skip inode-dedup there.
3. *(Low priority)* Optionally skip the chunk-hash pass and hash candidates in full directly — marginally faster only when most same-size files are genuine duplicates.
