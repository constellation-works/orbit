use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_policy_resource;
use orbit_store::compose::{
    global_policy_def_store, layered_policy_def_store, workspace_policy_def_store,
};
use orbit_types::policy::{FsProfile, PolicyDef, PolicyTimestamp};
use orbit_types::resource::{
    POLICY_RESOURCE_SCHEMA_VERSION, PolicyResource, PolicyResourceSpec, ResourceKind,
    ResourceMetadata,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tempfile::tempdir;

fn baseline_def(name: &str) -> PolicyDef {
    let now = PolicyTimestamp::try_from(std::time::SystemTime::now()).expect("policy timestamp");
    PolicyDef {
        name: name.to_string(),
        description: Some("test policy".to_string()),
        deny_read: Vec::new(),
        deny_modify: Vec::new(),
        fs_profiles: HashMap::new(),
        created_at: Some(now),
        updated_at: Some(now),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LegacyPolicyResourceSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(rename = "denyRead", default, skip_serializing_if = "Vec::is_empty")]
    deny_read: Vec<String>,
    #[serde(rename = "denyModify", default, skip_serializing_if = "Vec::is_empty")]
    deny_modify: Vec<String>,
    #[serde(
        rename = "fsProfiles",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    fs_profiles: HashMap<String, FsProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
}

#[test]
fn rejects_traversal_policy_name_without_external_write() {
    if !crate::support::isolated("policy::rejects_traversal_policy_name_without_external_write") {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let store = global_policy_def_store(dir.path().join("policies"));

    let err = store
        .upsert_policy_def(&baseline_def("../x"))
        .expect_err("traversal name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());

    let err = store
        .get_policy_def("../x")
        .expect_err("traversal lookup must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

#[test]
fn rejects_traversal_policy_metadata_name_when_loading() {
    if !crate::support::isolated("policy::rejects_traversal_policy_metadata_name_when_loading") {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let policies_dir = dir.path().join("policies");
    std::fs::create_dir_all(&policies_dir).expect("mkdir");
    std::fs::write(
        policies_dir.join("bad.yaml"),
        "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: ../x\nspec: {}\n",
    )
    .expect("seed");

    let store = global_policy_def_store(policies_dir);
    let err = store
        .list_policy_defs()
        .expect_err("traversal metadata name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());
}

#[test]
fn policy_timestamps_preserve_legacy_records_offsets_precision_merges_and_reject_invalid_inputs() {
    if !crate::support::isolated(
        "policy::policy_timestamps_preserve_legacy_records_offsets_precision_merges_and_reject_invalid_inputs",
    ) {
        return;
    }

    type LegacyPolicyResource = orbit_types::resource::ResourceEnvelope<LegacyPolicyResourceSpec>;

    let valid_inputs = [
        // Canonical UTC & fractional precisions (AutoSi: 0, 3, 6, 9 digits; >9 truncated)
        ("2026-11-18T05:12:14Z", "2026-11-18T05:12:14Z"),
        ("2026-11-18T05:12:14.0Z", "2026-11-18T05:12:14Z"),
        ("2026-11-18T05:12:14.000000000Z", "2026-11-18T05:12:14Z"),
        ("2026-11-18T05:12:14.1Z", "2026-11-18T05:12:14.100Z"),
        ("2026-11-18T05:12:14.12Z", "2026-11-18T05:12:14.120Z"),
        ("2026-11-18T05:12:14.123Z", "2026-11-18T05:12:14.123Z"),
        ("2026-11-18T05:12:14.123000Z", "2026-11-18T05:12:14.123Z"),
        ("2026-11-18T05:12:14.1234Z", "2026-11-18T05:12:14.123400Z"),
        ("2026-11-18T05:12:14.123456Z", "2026-11-18T05:12:14.123456Z"),
        (
            "2026-11-18T05:12:14.123456000Z",
            "2026-11-18T05:12:14.123456Z",
        ),
        (
            "2026-11-18T05:12:14.1234567Z",
            "2026-11-18T05:12:14.123456700Z",
        ),
        (
            "2026-11-18T05:12:14.123456789Z",
            "2026-11-18T05:12:14.123456789Z",
        ),
        (
            "2026-11-18T05:12:14.123456789999Z",
            "2026-11-18T05:12:14.123456789Z",
        ),
        ("2026-11-18T05:12:14.000000000452Z", "2026-11-18T05:12:14Z"),
        // Fixed offsets, boundary rollovers, leap-day rollover, Unicode minus (U+2212), compact/spaced offsets
        ("2026-11-18T05:12:14+00:00", "2026-11-18T05:12:14Z"),
        ("2026-11-18T05:12:14-00:00", "2026-11-18T05:12:14Z"),
        ("2026-11-18T10:42:14+05:30", "2026-11-18T05:12:14Z"),
        ("2026-11-17T21:12:14-08:00", "2026-11-18T05:12:14Z"),
        ("2026-11-17T21:12:14−08:00", "2026-11-18T05:12:14Z"),
        ("2026-11-18T10:42:14+0530", "2026-11-18T05:12:14Z"),
        ("2026-11-17T21:12:14-0800", "2026-11-18T05:12:14Z"),
        ("2026-11-18T10:42:14+05: 30", "2026-11-18T05:12:14Z"),
        ("2026-01-01T00:30:00+02:00", "2025-12-31T22:30:00Z"),
        ("2025-12-31T22:30:00-02:00", "2026-01-01T00:30:00Z"),
        ("2024-03-01T00:15:00+01:00", "2024-02-29T23:15:00Z"),
        ("2026-11-18T23:59:00+23:59", "2026-11-18T00:00:00Z"),
        ("2026-11-18T00:00:00-23:59", "2026-11-18T23:59:00Z"),
        // Pinned chrono relaxed RFC 3339 compatibility cases: lowercase t/z, space separator, UTC suffix, unpadded fields, leap seconds, extended years
        ("2026-11-18t05:12:14z", "2026-11-18T05:12:14Z"),
        ("2026-11-18 05:12:14Z", "2026-11-18T05:12:14Z"),
        ("2026-11-18 05:12:14 UTC", "2026-11-18T05:12:14Z"),
        ("2026-11-18 05:12:14 utc", "2026-11-18T05:12:14Z"),
        ("2026-1-2T3:4:5Z", "2026-01-02T03:04:05Z"),
        (
            "2015-06-30T23:59:59.999999999Z",
            "2015-06-30T23:59:59.999999999Z",
        ),
        ("2015-06-30T23:59:60Z", "2015-06-30T23:59:60Z"),
        ("2015-06-30T23:59:60.500Z", "2015-06-30T23:59:60.500Z"),
        ("2015-07-01T01:59:60.250+02:00", "2015-06-30T23:59:60.250Z"),
        ("2015-07-01T00:00:00Z", "2015-07-01T00:00:00Z"),
        ("1970-01-01T00:00:60Z", "1970-01-01T00:00:60Z"),
        ("0000-01-01T00:00:00Z", "0000-01-01T00:00:00Z"),
        ("-0001-12-31T23:59:59Z", "-0001-12-31T23:59:59Z"),
        ("+10000-01-01T00:00:00Z", "+10000-01-01T00:00:00Z"),
        ("-262143-01-01T00:00:00Z", "-262143-01-01T00:00:00Z"),
        (
            "+262142-12-31T23:59:59.999999999Z",
            "+262142-12-31T23:59:59.999999999Z",
        ),
    ];

    let mut parsed_pairs: Vec<(DateTime<Utc>, PolicyTimestamp, &str)> = Vec::new();
    for (raw, expected_canonical) in valid_inputs {
        let json_str = serde_json::to_string(raw).expect("json string");
        let old_dt: DateTime<Utc> =
            serde_json::from_str(&json_str).unwrap_or_else(|e| panic!("chrono parse {raw}: {e}"));
        let new_ts: PolicyTimestamp =
            serde_json::from_str(&json_str).unwrap_or_else(|e| panic!("policy parse {raw}: {e}"));

        let old_json = serde_json::to_string(&old_dt).expect("chrono serialize");
        let new_json = serde_json::to_string(&new_ts).expect("policy serialize");
        assert_eq!(new_json, old_json, "JSON serialization parity for {raw}");
        assert_eq!(
            serde_json::from_str::<PolicyTimestamp>(&new_json).expect("read serialized timestamp"),
            new_ts,
            "timestamp must survive serialization for {raw}"
        );
        assert_eq!(
            new_json,
            serde_json::to_string(expected_canonical).expect("serialize canonical timestamp"),
            "canonical RFC 3339 for {raw}"
        );
        assert_eq!(
            format!("{new_ts:?}"),
            format!("{old_dt:?}"),
            "Debug parity for {raw}"
        );
        assert_eq!(
            format!("{new_ts}"),
            format!("{old_dt}"),
            "Display parity for {raw}"
        );

        // Compare full PolicyResource YAML & JSON serialization between legacy chrono and new PolicyTimestamp.
        let yaml_input = format!(
            "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: sample\nspec:\n  description: parity\n  created_at: {json_str}\n  updated_at: {json_str}\n"
        );
        let legacy_doc: LegacyPolicyResource =
            serde_yaml::from_str(&yaml_input).expect("legacy yaml parse");
        let new_doc = parse_policy_resource(&yaml_input, "sample").expect("new yaml parse");

        assert_eq!(
            serde_yaml::to_string(&new_doc).expect("new yaml dump"),
            serde_yaml::to_string(&legacy_doc).expect("legacy yaml dump"),
            "YAML PolicyResource serialization parity for {raw}"
        );
        assert_eq!(
            serde_json::to_string(&new_doc).expect("new json dump"),
            serde_json::to_string(&legacy_doc).expect("legacy json dump"),
            "JSON PolicyResource serialization parity for {raw}"
        );

        parsed_pairs.push((old_dt, new_ts, raw));
    }

    // Verify pairwise ordering and equality match chrono across all valid cases (including offsets and leap seconds).
    for (old_a, new_a, raw_a) in &parsed_pairs {
        for (old_b, new_b, raw_b) in &parsed_pairs {
            assert_eq!(
                new_a.cmp(new_b),
                old_a.cmp(old_b),
                "ordering mismatch between {raw_a} and {raw_b}"
            );
            assert_eq!(
                new_a == new_b,
                old_a == old_b,
                "equality mismatch between {raw_a} and {raw_b}"
            );
        }
    }

    // Absent and null timestamp fields: omitted when serialized, accepted when absent or null.
    for yaml_variant in [
        "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: absent\nspec:\n  description: none\n",
        "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: absent\nspec:\n  description: none\n  created_at: null\n  updated_at: null\n",
    ] {
        let legacy: LegacyPolicyResource =
            serde_yaml::from_str(yaml_variant).expect("legacy absent/null");
        let resource = parse_policy_resource(yaml_variant, "absent").expect("new absent/null");
        assert!(resource.spec.created_at.is_none());
        assert!(resource.spec.updated_at.is_none());
        let serialized_new = serde_yaml::to_string(&resource).expect("serialize new");
        let serialized_legacy = serde_yaml::to_string(&legacy).expect("serialize legacy");
        assert_eq!(serialized_new, serialized_legacy);
        assert!(!serialized_new.contains("created_at"));
        assert!(!serialized_new.contains("updated_at"));
    }

    // Invalid inputs: both chrono and PolicyTimestamp/parse_policy_resource reject them.
    let invalid_inputs = [
        "",
        "not-a-timestamp",
        "2026-02-29T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-00-10T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2100-02-29T00:00:00Z",
        "2026-11-18T24:00:00Z",
        "2026-11-18T25:00:00Z",
        "2026-11-18T12:60:00Z",
        "2026-11-18T12:00:61Z",
        "2026-11-18T05:12:14.Z",
        "2026-11-18T05:12:14",
        "2026-11-18T05:12:14+24:00",
        "2026-11-18T05:12:14-24:00",
        "2026-11-18T05:12:14+08:60",
        "2026-11-18T05:12:14+08",
        "2026-11-18T05:12:14+08:0",
        "2026-11-18T05:12:14GMT",
        "2026-11-18T05:12:14Zulu",
        "2026/11/18T05:12:14Z",
        "2026-11-18_05:12:14Z",
        "2026-11-18T05:12:14Z trailing",
        "10000-01-01T00:00:00Z",
        "-262144-01-01T00:00:00Z",
        "+262143-01-01T00:00:00Z",
        "-262143-01-01T00:00:00+01:00",
        "+262142-12-31T23:30:00-01:00",
    ];
    for bad in invalid_inputs {
        let json_str = serde_json::to_string(bad).expect("json string");
        assert!(
            serde_json::from_str::<DateTime<Utc>>(&json_str).is_err(),
            "chrono unexpectedly accepted {bad:?}"
        );
        assert!(
            serde_json::from_str::<PolicyTimestamp>(&json_str).is_err(),
            "PolicyTimestamp must reject {bad:?}"
        );
        let bad_yaml = format!(
            "schemaVersion: 2\nkind: Policy\nmetadata:\n  name: bad\nspec:\n  created_at: {json_str}\n"
        );
        assert!(
            parse_policy_resource(&bad_yaml, "bad").is_err(),
            "parse_policy_resource must reject {bad:?}"
        );
    }

    // Focused file-store & layered-store round trips for legacy (pre-existing YAML with offsets/fractions) and newly written records.
    let dir = tempdir().expect("tempdir");
    let global_dir = dir.path().join("global-policies");
    let workspace_dir = dir.path().join("workspace-policies");
    std::fs::create_dir_all(&global_dir).expect("mkdir global");
    std::fs::create_dir_all(&workspace_dir).expect("mkdir workspace");

    // Seed a legacy policy file written with non-zero offset and microsecond precision.
    std::fs::write(
        global_dir.join("default.yaml"),
        concat!(
            "schemaVersion: 2\n",
            "kind: Policy\n",
            "metadata:\n",
            "  name: default\n",
            "spec:\n",
            "  description: global policy\n",
            "  denyRead:\n",
            "    - \"**/.env\"\n",
            "  denyModify:\n",
            "    - .orbit/**\n",
            "    - \"!.orbit/tmp/**\"\n",
            "  fsProfiles:\n",
            "    implementer:\n",
            "      read:\n",
            "        - ./**\n",
            "      modify:\n",
            "        - ./**\n",
            "  created_at: \"2026-11-17T20:00:00.123456-05:00\"\n",
            "  updated_at: \"2026-11-18T02:00:00Z\"\n",
        ),
    )
    .expect("seed legacy global policy");

    let global_store = global_policy_def_store(global_dir.clone());
    let loaded_global = global_store
        .get_policy_def("default")
        .expect("get global")
        .expect("default exists");
    assert_eq!(
        loaded_global.created_at,
        Some("2026-11-18T01:00:00.123456Z".parse().unwrap())
    );
    assert_eq!(
        loaded_global.updated_at,
        Some("2026-11-18T02:00:00Z".parse().unwrap())
    );

    // Upsert a workspace override with a newer updated_at and verify file-store serialization + layered merge.
    let workspace_store = workspace_policy_def_store(workspace_dir.clone());
    let mut workspace_def = loaded_global.clone();
    workspace_def.description = Some("workspace override".to_string());
    workspace_def.created_at = Some("2026-11-18T03:00:00Z".parse().unwrap());
    workspace_def.updated_at = Some("2026-11-18T04:30:00.789Z".parse().unwrap());
    workspace_store
        .upsert_policy_def(&workspace_def)
        .expect("upsert workspace policy");

    let written_yaml =
        std::fs::read_to_string(workspace_dir.join("default.yaml")).expect("read written yaml");
    let legacy_parsed: LegacyPolicyResource = serde_yaml::from_str(&written_yaml)
        .expect("legacy chrono reader parses newly written yaml");
    assert_eq!(
        legacy_parsed.spec.created_at,
        Some("2026-11-18T03:00:00Z".parse().unwrap())
    );
    assert_eq!(
        legacy_parsed.spec.updated_at,
        Some("2026-11-18T04:30:00.789Z".parse().unwrap())
    );

    let layered = layered_policy_def_store(workspace_store, global_store);
    let merged = layered
        .get_policy_def("default")
        .expect("get layered")
        .expect("merged default");
    // PolicyDef::merged keeps global.created_at.or(workspace.created_at) and workspace.updated_at.max(global.updated_at).
    assert_eq!(merged.created_at, loaded_global.created_at);
    assert_eq!(merged.updated_at, workspace_def.updated_at);
    assert_eq!(merged.description.as_deref(), Some("workspace override"));
    assert_eq!(
        layered.list_policy_defs().expect("list layered"),
        vec![merged]
    );

    // Also verify PolicyResource direct struct round-trip.
    let resource = PolicyResource {
        schema_version: POLICY_RESOURCE_SCHEMA_VERSION,
        kind: ResourceKind::Policy,
        metadata: ResourceMetadata::named("direct"),
        spec: PolicyResourceSpec {
            description: None,
            deny_read: Vec::new(),
            deny_modify: Vec::new(),
            fs_profiles: HashMap::new(),
            created_at: loaded_global.created_at,
            updated_at: workspace_def.updated_at,
        },
    };
    let encoded = serde_yaml::to_string(&resource).expect("encode resource");
    let decoded = parse_policy_resource(&encoded, "direct").expect("decode resource");
    assert_eq!(decoded, resource);
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[test]
fn policy_timestamp_rejects_out_of_range_system_times() {
    use std::time::{Duration, UNIX_EPOCH};

    let outside_policy_range = Duration::from_secs(10_000_000_000_000);
    assert!(PolicyTimestamp::try_from(UNIX_EPOCH + outside_policy_range).is_err());
    assert!(PolicyTimestamp::try_from(UNIX_EPOCH - outside_policy_range).is_err());
    assert_eq!(
        PolicyTimestamp::try_from(UNIX_EPOCH - Duration::from_millis(1)).expect("pre-epoch"),
        "1969-12-31T23:59:59.999Z".parse().expect("timestamp")
    );
}
