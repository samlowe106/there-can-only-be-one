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

Duplicate checking happens in two passes:

**The first pass** quickly and inexpensively rules out true negatives. All files in the directory are `stat`ed and bucketed by file size, then a small amount of the file (based on the disk's block size) is streamed into the [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) hashing algorithm, so the file contents are never held in memory. (These samples are taken from the start of the file by default, but multiple sampling strategies can be specified available.) These hashes are used to build a hashmap, where the values are vectors of files that hash to that key. This allows for only $O(n)$ comparisons, compared to the the naive $\Omega (n^2)$ number of comparisons.

**The second pass** computes the full hash of each file, then checks them against the full hash of each other file that they collided with from the first pass. (Files small enough to have been read in full during the first pass reuse that hash.) This pass finds all true positives, which are then reported to the user.

The end results are:
* Files larger than available memory can be hashed without issue
* Size bucketing and file sampling in the first pass immediately narrow down the possible candidates very quickly, often eliminating 95% of possible duplicates in practice at minimal performance overhead
* Obtaining file samples take only one disk read, so the first pass samples and hashes each file as quickly as possible. Files that can be fully hashed with just this read retain these hashes if they continue on to the second pass
* False positives are only possible if two files have identical sizes and BLAKE3 hashes on both their sample and full file contents, which is so unlikely that that it can be ruled out in practice
* [rayon](https://github.com/rayon-rs/rayon) threading library lets hashes run in parallel across files, providing massive performance gain in both passes
* On Unix, hardlinks are deduplicated by `(device, inode)` so multiple names for the same physical file are not reported as duplicates. (This feature isn't available for other platforms in stable Rust yet, so hardlink dedup is skipped there.)
* Unreadable files and directory-walk errors are logged to `stderr` and skipped rather than aborting the scan.

Overall runtime is $O(n)$ metadata work + $O(H)$ bytes hashed, where $n$ is the number of files. $H$ is technically $O(B)$ where $B$ is the total size of the files in bytes, but $H$ is often only $O(n)$ in practice because we prune by filesize and prefix hashing — only `stat`ing or read a constant-sized amount once from each file.

Memory is $O(n)$ for the file metadata plus a constant per in-flight hash (because files are streamed, not loaded), which is notably _independent of individual file sizes_. The two hashing passes are spread across cores with rayon, so the byte-bound work scales with available parallelism until it saturates disk I/O.

## TODOs

1. **Opt-in delete mode.** Add an action to reclaim space (delete redundant copies, or replace them with hard/symlinks), with report-only remaining the default and deletion gated behind an explicit flag plus a keep-policy for which copy survives. However, choosing which duplicate to delete will likely require user input and would be better done in a program with a UI
2. **Windows hardlink dedup.** The tool now builds and runs cross-platform, but hardlink dedup is Unix-only: reading a file's identity on Windows needs `Metadata::volume_serial_number`/`file_index`, which are gated behind the still-unstable `windows_by_handle` feature ([rust-lang/rust#63010](https://github.com/rust-lang/rust/issues/63010)). We tried using them and it broke the Windows build (nightly-only), so `platform::physical_id` returns `None` off-Unix for now. Revisit when that feature stabilizes (then Windows gets dedup for free), or implement it via `GetFileInformationByHandle` if it's needed sooner
3. *(Low priority)* Optionally skip the chunk-hash pass and hash candidates in full directly — marginally faster only when most same-size files are genuine duplicates