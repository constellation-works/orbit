#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use std::fs;
use std::path::{Path, PathBuf};

use orbit_types::task::{ArtifactManifestV2, TaskCommentRowV2, TaskEnvelopeV2, TaskEventRowV2};

#[test]
fn task_artifact_v2_fixtures_parse_and_validate() {
    for fixture in ["minimal", "relations", "comments"] {
        validate_fixture(fixture);
    }
}

fn validate_fixture(name: &str) {
    let root = fixture_root().join(name);
    let envelope = read_yaml::<TaskEnvelopeV2>(&root.join("task.yaml"));
    envelope.validate().expect("fixture task envelope");

    for event in read_jsonl::<TaskEventRowV2>(&root.join("events.jsonl")) {
        event.validate().expect("fixture task event row");
    }

    for comment in read_jsonl::<TaskCommentRowV2>(&root.join("comments.jsonl")) {
        comment.validate().expect("fixture task comment row");
    }

    for document in [
        "description.md",
        "acceptance.md",
        "plan.md",
        "execution-summary.md",
    ] {
        assert!(
            root.join(document).is_file(),
            "fixture {name} should include {document}"
        );
    }

    let artifact_manifest = root.join("artifacts").join("manifest.yaml");
    if artifact_manifest.is_file() {
        read_yaml::<ArtifactManifestV2>(&artifact_manifest)
            .validate()
            .expect("fixture artifact manifest");
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("task-artifacts-v2")
}

fn read_yaml<T>(path: &Path) -> T
where
    T: serde::de::DeserializeOwned,
{
    let raw = fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("read fixture {}: {err}", path.display());
    });
    serde_yaml::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse fixture {}: {err}", path.display());
    })
}

fn read_jsonl<T>(path: &Path) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    let raw = fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("read fixture {}: {err}", path.display());
    });
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).unwrap_or_else(|err| {
                panic!("parse fixture {} line {}: {err}", path.display(), index + 1);
            })
        })
        .collect()
}
