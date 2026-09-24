use std::io::Cursor;

use crate::fs::reverse_lines::ReverseLines;

fn lines(raw: &str, block_size: usize) -> Vec<String> {
    ReverseLines::with_block_size(Cursor::new(raw.as_bytes()), block_size)
        .expect("open")
        .collect::<Result<_, _>>()
        .expect("read lines")
}

#[test]
fn yields_lines_newest_first_for_every_block_size() {
    let raw = "first\r\n\nthird line\nlast";
    for block_size in [1, 2, 3, 7, 64 * 1024] {
        assert_eq!(
            lines(raw, block_size),
            ["last", "third line", "", "first"],
            "block size {block_size}"
        );
    }
}

#[test]
fn a_trailing_newline_ends_the_last_line_rather_than_adding_one() {
    for block_size in [1, 4, 1024] {
        assert_eq!(lines("a\nb\n", block_size), ["b", "a"]);
        assert_eq!(lines("\n", block_size), [""]);
    }
    assert!(lines("", 4).is_empty());
}

#[test]
fn invalid_utf8_is_an_error_only_when_reached() {
    let raw = b"\xff\xfe\nok\n".to_vec();
    let mut reader = ReverseLines::with_block_size(Cursor::new(raw), 3).expect("open");
    assert_eq!(reader.next().expect("line").expect("utf8"), "ok");
    let error = reader.next().expect("line").expect_err("invalid utf8");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(reader.next().is_none());
}
