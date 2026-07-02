use there_can_only_be_one::output::human_readable_bytes;

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
