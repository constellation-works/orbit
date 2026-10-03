use std::collections::HashMap;

use crate::policy::{FsOperation, FsProfile, PolicyDef};

/// `secrets/*` matches a newline inside the filename. The appended
/// `denyRead: secrets/**` has to match it too, and settle the path the same
/// way it settles an ordinary sibling.
#[test]
fn a_single_star_read_with_recursive_deny_rejects_a_newline_filename() {
    let mut fs_profiles = HashMap::new();
    fs_profiles.insert(
        "agent".to_string(),
        FsProfile {
            read: vec!["secrets/*".to_string(), "notes/**".to_string()],
            modify: Vec::new(),
        },
    );
    let def = PolicyDef {
        name: "newline-glob".to_string(),
        description: None,
        deny_read: vec!["secrets/**".to_string()],
        deny_modify: Vec::new(),
        fs_profiles,
        created_at: None,
        updated_at: None,
    };

    let newline = def
        .check_path("agent", FsOperation::Read, "secrets/a\nb")
        .expect("newline name");
    let sibling = def
        .check_path("agent", FsOperation::Read, "secrets/ab")
        .expect("ordinary sibling");
    assert!(!newline.allowed, "{newline:?}");
    assert!(!sibling.allowed, "{sibling:?}");
    assert_eq!(newline.matched_rule, "secrets/**");
    assert_eq!(sibling.matched_rule, newline.matched_rule);

    let allowed = def
        .check_path("agent", FsOperation::Read, "notes/a\nb")
        .expect("positive recursive");
    assert!(allowed.allowed, "{allowed:?}");
    assert_eq!(allowed.matched_rule, "notes/**");
}
