use std::fs;
use std::time::Duration;

use tempfile::tempdir;
use there_can_only_be_one::dedupe::DupGroup;
use there_can_only_be_one::output::{human_readable_bytes, write_log};

#[test]
fn formats_bytes_with_binary_units() {
    assert_eq!(human_readable_bytes(0), "0 B");
    assert_eq!(human_readable_bytes(512), "512 B");
    assert_eq!(human_readable_bytes(1023), "1023 B"); // stays in bytes below 1 KiB
    assert_eq!(human_readable_bytes(1024), "1.00 KiB"); // exact unit boundary
    assert_eq!(human_readable_bytes(1536), "1.50 KiB"); // 1.5 KiB
    assert_eq!(human_readable_bytes(1024 * 1024), "1.00 MiB");
    assert_eq!(human_readable_bytes(1_576_587_128), "1.47 GiB"); // the ~Pictures run
    assert_eq!(human_readable_bytes(5 * 1024_u64.pow(4)), "5.00 TiB");
}

#[test]
fn write_log_includes_duplicates_and_empties() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("report.log");
    let groups = vec![DupGroup {
        size: 3,
        paths: vec!["/x/a".into(), "/x/b".into()],
    }];
    let empties = vec!["/x/empty1".into()];

    write_log(&log, 5, 2, Duration::from_millis(0), &groups, &empties).unwrap();

    let contents = fs::read_to_string(&log).unwrap();
    assert!(contents.contains("Duplicates (3 bytes each):"));
    assert!(contents.contains("/x/a"));
    assert!(contents.contains("/x/b")); // the duplicate group is in the log
    assert!(contents.contains("/x/empty1")); // and so are the empties
}
