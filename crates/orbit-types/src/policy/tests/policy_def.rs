use std::collections::HashMap;

use chrono::{TimeZone, Utc};

use crate::policy::{FsOperation, FsProfile, PolicyDef};
use crate::resource::PolicyResource;

const POLICY_RESOURCE_YAML_WITHOUT_TIMESTAMPS: &str = r#"
schemaVersion: 2
kind: Policy
metadata:
  name: test-policy
spec:
  description: A test policy without timestamps
  denyRead:
    - "**/.env"
  denyModify:
    - "**/.git/**"
  fsProfiles:
    developer:
      read:
        - ./**
      modify:
        - src/**
"#;

#[test]
fn parsing_policy_resource_without_created_at_twice_yields_equal_structs() {
    let first: PolicyResource =
        serde_yaml::from_str(POLICY_RESOURCE_YAML_WITHOUT_TIMESTAMPS).expect("first parse");
    let second: PolicyResource =
        serde_yaml::from_str(POLICY_RESOURCE_YAML_WITHOUT_TIMESTAMPS).expect("second parse");

    // Must be deterministic and equal across independent parses.
    assert_eq!(first, second);
    assert_eq!(first.spec.created_at, None);
    assert_eq!(first.spec.updated_at, None);

    // Round-trip test: serializing and re-deserializing preserves equivalence
    // and omits absent timestamps.
    let serialized = serde_yaml::to_string(&first).expect("serialize policy resource");
    assert!(
        !serialized.contains("created_at"),
        "absent created_at should not be serialized: {serialized}"
    );
    assert!(
        !serialized.contains("updated_at"),
        "absent updated_at should not be serialized: {serialized}"
    );

    let roundtripped: PolicyResource =
        serde_yaml::from_str(&serialized).expect("deserialize roundtripped policy resource");
    assert_eq!(first, roundtripped);
}

#[test]
fn parsing_policy_resource_with_explicit_timestamps_roundtrips() {
    let timestamp = Utc.with_ymd_and_hms(2026, 9, 8, 12, 0, 0).unwrap();
    let yaml = format!(
        r#"
schemaVersion: 2
kind: Policy
metadata:
  name: test-explicit
spec:
  description: Policy with explicit timestamps
  created_at: {}
  updated_at: {}
"#,
        timestamp.to_rfc3339(),
        timestamp.to_rfc3339()
    );

    let parsed: PolicyResource = serde_yaml::from_str(&yaml).expect("parse policy resource");
    assert_eq!(parsed.spec.created_at, Some(timestamp));
    assert_eq!(parsed.spec.updated_at, Some(timestamp));

    let serialized = serde_yaml::to_string(&parsed).expect("serialize");
    assert!(serialized.contains("created_at:"));
    assert!(serialized.contains("updated_at:"));

    let roundtripped: PolicyResource =
        serde_yaml::from_str(&serialized).expect("deserialize roundtrip");
    assert_eq!(parsed, roundtripped);
}

#[test]
fn parsing_policy_def_without_created_at_twice_yields_equal_structs() {
    let yaml = r#"
name: test-def
description: Def without timestamps
denyRead:
  - "**/.env"
"#;
    let first: PolicyDef = serde_yaml::from_str(yaml).expect("first parse");
    let second: PolicyDef = serde_yaml::from_str(yaml).expect("second parse");

    assert_eq!(first, second);
    assert_eq!(first.created_at, None);
    assert_eq!(first.updated_at, None);

    let serialized = serde_yaml::to_string(&first).expect("serialize");
    assert!(!serialized.contains("created_at"));
    assert!(!serialized.contains("updated_at"));

    let roundtripped: PolicyDef = serde_yaml::from_str(&serialized).expect("roundtrip parse");
    assert_eq!(first, roundtripped);
}

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
