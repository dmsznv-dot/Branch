use media_stream::{ByteRange, RangeError::*, resolve_range};

#[test]
fn resolves_single_ranges_and_outer_http_whitespace() {
    for (header, start, len) in [
        ("bytes=0-0", 0, 1),
        ("bytes=9-", 9, 1),
        ("bytes=-3", 7, 3),
        ("bytes=7-999", 7, 3),
        ("bytes=-99", 0, 10),
        (" \tbytes=0-9\t ", 0, 10),
        ("bytes=000-001", 0, 2),
    ] {
        assert_eq!(
            resolve_range(Some(header), 10),
            Ok(Some(ByteRange { start, len })),
            "{header}"
        );
    }
}

#[test]
fn absent_header_means_full_response_even_for_empty_media() {
    for size in [0, 10, u64::MAX] {
        assert_eq!(resolve_range(None, size), Ok(None));
    }
}

#[test]
fn unsatisfiable_ranges() {
    for header in ["bytes=10-", "bytes=-0", "bytes=8-7", "bytes=10-10"] {
        assert_eq!(
            resolve_range(Some(header), 10),
            Err(Unsatisfiable),
            "{header}"
        );
    }
}

#[test]
fn empty_media_has_no_satisfiable_range() {
    for header in ["bytes=0-0", "bytes=0-", "bytes=-1", "bytes=-0", "bytes=8-7"] {
        assert_eq!(
            resolve_range(Some(header), 0),
            Err(Unsatisfiable),
            "{header}"
        );
    }
}

#[test]
fn multiple_ranges_are_distinct_from_malformed_values() {
    assert_eq!(resolve_range(Some("bytes=0-1,5-6"), 10), Err(Multiple));
}

#[test]
fn rejects_unknown_units_internal_whitespace_signs_and_overflow() {
    for header in [
        "",
        " ",
        "items=0-1",
        "Bytes=0-1",
        "bytes =0-1",
        "bytes= 0-1",
        "bytes=0 -1",
        "bytes=0-1  ",
        "bytes=0-1\r\n",
        "bytes=0-1-2",
        "bytes=--3",
        "bytes=+0-1",
        "bytes=0-+1",
        "bytes=-+1",
        "bytes=-",
        "bytes=0",
        "bytes=18446744073709551616-",
        "bytes=0-18446744073709551616",
        "bytes=-18446744073709551616",
        "bytes=０-1",
        "bytes=0-a",
    ] {
        assert_eq!(
            resolve_range(Some(header), 10),
            Err(Malformed),
            "{header:?}"
        );
    }
}

#[test]
fn u64_boundaries_do_not_overflow() {
    for (header, start, len) in [
        ("bytes=0-18446744073709551615", 0, u64::MAX),
        (
            "bytes=18446744073709551614-18446744073709551615",
            u64::MAX - 1,
            1,
        ),
        ("bytes=-18446744073709551615", 0, u64::MAX),
    ] {
        assert_eq!(
            resolve_range(Some(header), u64::MAX),
            Ok(Some(ByteRange { start, len }))
        );
    }
    assert_eq!(
        resolve_range(Some("bytes=18446744073709551615-"), u64::MAX),
        Err(Unsatisfiable)
    );
}
