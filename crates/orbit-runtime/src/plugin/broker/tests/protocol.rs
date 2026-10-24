use std::io::Cursor;

use super::super::protocol::{FrameError, MAX_REQUEST_BYTES, read_frame};

#[test]
fn an_oversized_frame_is_refused_before_its_body_is_read() {
    let declared = MAX_REQUEST_BYTES + 1;
    let mut wire = declared.to_be_bytes().to_vec();
    wire.extend_from_slice(b"body bytes the broker must not consume");
    let mut reader = Cursor::new(wire);

    let error = read_frame(&mut reader, MAX_REQUEST_BYTES).expect_err("frame over the cap");

    assert!(
        matches!(error, FrameError::TooLarge { declared: seen } if seen == declared),
        "unexpected frame error: {error:?}"
    );
    assert_eq!(
        reader.position(),
        4,
        "only the length prefix may be consumed from an oversized request"
    );
}
