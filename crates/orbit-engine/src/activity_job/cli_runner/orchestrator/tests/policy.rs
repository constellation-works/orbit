#![allow(missing_docs)]

use super::super::policy::resolved_activity_fs_profile_name;

#[test]
fn cli_activity_fs_profile_resolver_preserves_named_profile() {
    assert_eq!(resolved_activity_fs_profile_name(None), "unrestricted");
    assert_eq!(
        resolved_activity_fs_profile_name(Some("implementer")),
        "implementer"
    );
}
