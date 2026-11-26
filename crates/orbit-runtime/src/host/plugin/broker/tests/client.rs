use std::io::ErrorKind;

use super::super::client::require_same_uid;

#[test]
fn a_different_server_uid_is_refused() {
    assert!(require_same_uid(1000, 1000).is_ok());
    assert_eq!(
        require_same_uid(1001, 1000)
            .expect_err("foreign uid")
            .kind(),
        ErrorKind::PermissionDenied
    );
}
