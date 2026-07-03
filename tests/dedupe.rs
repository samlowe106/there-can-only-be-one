use std::collections::HashMap;
use std::fs;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use tempfile::tempdir;
use there_can_only_be_one::dedupe::{
    DupGroup, FileInfo, Sample, ScanOptions, assemble_groups, bucket_by_size, chunk_hash,
    confirm_by_full_hash, duplicate_files, find_duplicates, reclaimable_bytes, take_empty_files,
};

// ---- helpers ---------------------------------------------------------------

/// Write `contents` to `<dir>/<name>`.
fn write(dir: &Path, name: &str, contents: &[u8]) {
    fs::write(dir.join(name), contents).unwrap();
}

/// Bucket `dir` by size with default options.
fn buckets(dir: &Path) -> HashMap<u64, Vec<FileInfo>> {
    bucket_by_size(dir, &ScanOptions::default())
}

/// Find duplicates in `dir` with default options.
fn dups(dir: &Path) -> Vec<DupGroup> {
    find_duplicates(dir, &ScanOptions::default())
}

/// Build a glob set from patterns.
fn globs(patterns: &[&str]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).unwrap());
    }
    builder.build().unwrap()
}

/// The file names in each group, for easy assertions.
fn names(groups: &[DupGroup]) -> Vec<Vec<String>> {
    groups
        .iter()
        .map(|g| {
            g.paths
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        })
        .collect()
}

/// Sorted lengths of every bucket in a grouping map — a fingerprint of how
/// files were grouped, without depending on the (opaque) hash keys.
fn bucket_lens<K, V>(map: &HashMap<K, Vec<V>>) -> Vec<usize> {
    let mut lens: Vec<usize> = map.values().map(Vec::len).collect();
    lens.sort_unstable();
    lens
}

/// Total number of files retained across a grouping map.
fn total<K, V>(map: &HashMap<K, Vec<V>>) -> usize {
    map.values().map(Vec::len).sum()
}

/// A pair of equal-sized buffers with an identical leading chunk that differ
/// only near the end — so they collide in the chunk pass but not the full pass.
fn prefix_colliding_pair() -> (Vec<u8>, Vec<u8>) {
    let a = vec![0u8; 256 * 1024];
    let mut b = a.clone();
    b[200 * 1024] = 1; // differ well past any block-sized chunk
    (a, b)
}

// ---- find_duplicates (end to end) ------------------------------------------

#[test]
fn finds_identical_files() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a", b"hello world");
    write(dir.path(), "b", b"hello world");
    write(dir.path(), "c", b"something else");

    let groups = dups(dir.path());
    assert_eq!(groups.len(), 1);
    assert_eq!(names(&groups), vec![vec!["a".to_string(), "b".to_string()]]);
}

#[test]
fn same_size_different_content_is_not_a_duplicate() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a", b"aaaaaaaa");
    write(dir.path(), "b", b"bbbbbbbb"); // same length, different bytes

    assert!(dups(dir.path()).is_empty());
}

#[test]
fn large_files_beyond_the_chunk_are_confirmed_by_full_hash() {
    let dir = tempdir().unwrap();
    let (a, b) = prefix_colliding_pair();
    write(dir.path(), "a", &a);
    write(dir.path(), "b", &b);
    write(dir.path(), "c", &a); // true duplicate of a

    let groups = dups(dir.path());
    assert_eq!(groups.len(), 1);
    assert_eq!(names(&groups), vec![vec!["a".to_string(), "c".to_string()]]);
}

#[test]
fn empty_files_are_grouped_together() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a", b"");
    write(dir.path(), "b", b"");

    let groups = dups(dir.path());
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].paths.len(), 2);
}

// ---- bucket_by_size (pass 1) -----------------------------------------------

#[test]
fn bucket_by_size_groups_by_size_and_recurses_into_subdirs() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a", b"12345"); // size 5
    write(dir.path(), "b", b"67890"); // size 5, different content
    write(dir.path(), "d", b"xy"); // size 2
    fs::create_dir(dir.path().join("sub")).unwrap();
    write(&dir.path().join("sub"), "c", b"xyz"); // size 3, nested

    let buckets = buckets(dir.path());

    assert_eq!(total(&buckets), 4);
    assert_eq!(buckets[&5].len(), 2); // both size-5 files bucketed together
    assert!(buckets.contains_key(&3)); // recursed into the subdirectory
    assert!(buckets.contains_key(&2));
}

#[test]
fn bucket_by_size_collects_empty_files_under_the_zero_key() {
    // The CLI relies on this: empty files land in the size-0 bucket so they can
    // be split off and skipped before hashing.
    let dir = tempdir().unwrap();
    write(dir.path(), "e1", b"");
    write(dir.path(), "e2", b"");
    write(dir.path(), "nonempty", b"x");

    let buckets = buckets(dir.path());
    assert_eq!(buckets[&0].len(), 2);
    assert_eq!(buckets[&1].len(), 1);
}

// Hardlink dedup relies on a physical-file identity, which only Unix provides
// on stable Rust (see `platform::physical_id`).
#[cfg(unix)]
#[test]
fn bucket_by_size_deduplicates_hardlinks() {
    let dir = tempdir().unwrap();
    write(dir.path(), "orig", b"hello"); // size 5
    fs::hard_link(dir.path().join("orig"), dir.path().join("link")).unwrap();
    write(dir.path(), "other", b"world"); // size 5, distinct inode

    let buckets = buckets(dir.path());

    // orig and link share an inode, so only one of them is retained.
    assert_eq!(total(&buckets), 2);
    assert_eq!(buckets[&5].len(), 2);
}

// ---- filters ---------------------------------------------------------------

#[test]
fn min_size_skips_small_files() {
    let dir = tempdir().unwrap();
    write(dir.path(), "small_a", b"hi"); // size 2 duplicate pair
    write(dir.path(), "small_b", b"hi");
    write(dir.path(), "big_a", &[b'A'; 100]); // size 100 duplicate pair
    write(dir.path(), "big_b", &[b'A'; 100]);

    let opts = ScanOptions {
        min_size: 50,
        ..Default::default()
    };
    let groups = find_duplicates(dir.path(), &opts);

    // Only the 100-byte pair survives the size floor.
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].size, 100);
}

#[test]
fn exclude_glob_skips_matching_paths() {
    let dir = tempdir().unwrap();
    write(dir.path(), "keep_a", b"same"); // duplicate pair we want reported
    write(dir.path(), "keep_b", b"same");
    fs::create_dir(dir.path().join("skip")).unwrap();
    write(&dir.path().join("skip"), "dup", b"same"); // identical, but excluded

    let opts = ScanOptions {
        exclude: globs(&["**/skip/**"]),
        ..Default::default()
    };
    let groups = find_duplicates(dir.path(), &opts);

    // The excluded copy is never scanned, so only the two kept files group.
    assert_eq!(groups.len(), 1);
    assert_eq!(
        names(&groups),
        vec![vec!["keep_a".to_string(), "keep_b".to_string()]]
    );
}

// ---- symlink policy --------------------------------------------------------

#[cfg(unix)]
#[test]
fn follow_symlinks_controls_whether_linked_files_are_scanned() {
    // A file living outside the scanned tree, duplicated by a symlink inside it.
    let external = tempdir().unwrap();
    write(external.path(), "orig", b"shared contents");

    let dir = tempdir().unwrap();
    write(dir.path(), "copy", b"shared contents"); // distinct inode, same bytes
    std::os::unix::fs::symlink(external.path().join("orig"), dir.path().join("lnk")).unwrap();

    // Without following, the symlink is skipped: "copy" stands alone.
    assert!(find_duplicates(dir.path(), &ScanOptions::default()).is_empty());

    // Following resolves the link to a real, distinct-inode file, revealing the
    // duplicate.
    let opts = ScanOptions {
        follow_symlinks: true,
        ..Default::default()
    };
    let groups = find_duplicates(dir.path(), &opts);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].paths.len(), 2);
}

// ---- reporting helpers -----------------------------------------------------

#[test]
fn take_empty_files_removes_the_zero_bucket() {
    let dir = tempdir().unwrap();
    write(dir.path(), "e1", b"");
    write(dir.path(), "e2", b"");
    write(dir.path(), "nonempty", b"x");

    let mut b = buckets(dir.path());
    let empties = take_empty_files(&mut b);

    assert_eq!(empties.len(), 2);
    assert!(empties.iter().all(|p| p.extension().is_none())); // e1, e2
    assert!(!b.contains_key(&0)); // zero bucket removed
    assert!(b.contains_key(&1)); // other sizes untouched
}

#[test]
fn reclaimable_and_duplicate_files_counts() {
    let groups = vec![
        DupGroup {
            size: 100,
            paths: vec!["a".into(), "b".into(), "c".into()], // 3 files, 2 redundant
        },
        DupGroup {
            size: 10,
            paths: vec!["d".into(), "e".into()], // 2 files, 1 redundant
        },
    ];

    // Reclaimable = 100*2 + 10*1; files spanned = 3 + 2.
    assert_eq!(reclaimable_bytes(&groups), 210);
    assert_eq!(duplicate_files(&groups), 5);
    assert_eq!(reclaimable_bytes(&[]), 0);
    assert_eq!(duplicate_files(&[]), 0);
}

// ---- chunk_hash (pass 2) ---------------------------------------------------

#[test]
fn chunk_hash_drops_lonely_sizes_and_groups_by_prefix() {
    let dir = tempdir().unwrap();
    write(dir.path(), "dup1", b"identical"); // size 9
    write(dir.path(), "dup2", b"identical"); // size 9, same content
    write(dir.path(), "diff", b"different"); // size 9, different content
    write(dir.path(), "unique", b"a-size-of-its-own"); // lone size

    let chunks = chunk_hash(buckets(dir.path()), Sample::Start);

    // The lone-size file is never hashed; the two identical files share a
    // (size, chunk-hash) bucket, the odd one out sits alone.
    assert_eq!(total(&chunks), 3);
    assert_eq!(bucket_lens(&chunks), vec![1, 2]);
    assert!(chunks.keys().all(|(size, _)| *size == 9));
}

// ---- sample strategy -------------------------------------------------------

#[test]
fn sample_strategy_changes_which_bytes_are_read() {
    // Two same-size files that are identical except for a single byte at the
    // exact center. The prefix (and suffix) are byte-for-byte the same.
    let dir = tempdir().unwrap();
    let size = 512 * 1024;
    let mut a = vec![0u8; size];
    let mut b = vec![0u8; size];
    a[size / 2] = 1;
    b[size / 2] = 2;
    write(dir.path(), "a", &a);
    write(dir.path(), "b", &b);

    // Sampling the start reads the identical prefix, so they collide...
    let start = chunk_hash(buckets(dir.path()), Sample::Start);
    assert_eq!(bucket_lens(&start), vec![2]);

    // ...but sampling the middle reads the differing center byte and splits them.
    let middle = chunk_hash(buckets(dir.path()), Sample::Middle);
    assert_eq!(bucket_lens(&middle), vec![1, 1]);
}

#[test]
fn duplicates_are_found_under_every_sample_strategy() {
    // Sampling location is a performance knob — it must never change results.
    for sample in [Sample::Start, Sample::Middle, Sample::End] {
        let dir = tempdir().unwrap();
        write(dir.path(), "a", b"the very same contents");
        write(dir.path(), "b", b"the very same contents");
        write(dir.path(), "c", b"something else entirely");

        let opts = ScanOptions {
            sample,
            ..Default::default()
        };
        let groups = find_duplicates(dir.path(), &opts);
        assert_eq!(groups.len(), 1, "strategy {sample:?}");
        assert_eq!(names(&groups), vec![vec!["a".to_string(), "b".to_string()]]);
    }
}

// ---- confirm_by_full_hash (pass 3) -----------------------------------------

#[test]
fn confirm_by_full_hash_splits_prefix_collisions() {
    let dir = tempdir().unwrap();
    let (a, b) = prefix_colliding_pair();
    write(dir.path(), "a", &a);
    write(dir.path(), "b", &b);
    write(dir.path(), "c", &a); // true duplicate of a

    // All three collide in the chunk pass (identical leading chunk)...
    let chunks = chunk_hash(buckets(dir.path()), Sample::Start);
    assert_eq!(bucket_lens(&chunks), vec![3]);

    // ...but the full hash separates b out, leaving a+c together.
    let by_hash = confirm_by_full_hash(chunks);
    assert_eq!(bucket_lens(&by_hash), vec![1, 2]);
}

// ---- assemble_groups (pass 4) ----------------------------------------------

#[test]
fn assemble_groups_drops_singletons_and_orders_by_size() {
    let dir = tempdir().unwrap();
    // Two real duplicate pairs of different sizes.
    write(dir.path(), "big_b", &[b'A'; 300]);
    write(dir.path(), "big_a", &[b'A'; 300]);
    write(dir.path(), "small_a", b"hi");
    write(dir.path(), "small_b", b"hi");
    // A prefix-colliding-but-distinct pair: survives to `by_hash` as two
    // singleton buckets, which assemble_groups must drop.
    let (a, b) = prefix_colliding_pair();
    write(dir.path(), "lonely1", &a);
    write(dir.path(), "lonely2", &b);

    let by_hash = confirm_by_full_hash(chunk_hash(buckets(dir.path()), Sample::Start));
    // Sanity check the input actually contains singletons to drop.
    assert!(bucket_lens(&by_hash).contains(&1));

    let groups = assemble_groups(by_hash);

    // Singletons gone; groups ordered largest-first; paths sorted within.
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].size, 300);
    assert_eq!(
        names(&groups)[0],
        vec!["big_a".to_string(), "big_b".to_string()]
    );
    assert_eq!(groups[1].size, 2);
    assert_eq!(
        names(&groups)[1],
        vec!["small_a".to_string(), "small_b".to_string()]
    );
}
