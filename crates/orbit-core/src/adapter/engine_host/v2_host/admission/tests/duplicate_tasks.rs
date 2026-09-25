use super::super::duplicate_tasks::{canonical_text, covering_owner_from_message};

#[test]
fn canonical_text_preserves_token_boundaries() {
    let searchable = canonical_text("Update runtime in Cargo.lock");
    assert!(!searchable.contains(&canonical_text("time")));
    assert!(searchable.contains(&canonical_text("runtime")));
}

#[test]
fn covering_owner_parses_duplicate_and_covering_phrases() {
    assert_eq!(
        covering_owner_from_message(
            "ORB-11513",
            "Duplicate of active ORB-11511: both cite Wrangler 4.129.0.",
        )
        .as_deref(),
        Some("ORB-11511")
    );
    assert_eq!(
        covering_owner_from_message(
            "ORB-11526",
            "Covering implementation is active ORB-11511 (same Wrangler missing-name error).",
        )
        .as_deref(),
        Some("ORB-11511")
    );
    assert_eq!(
        covering_owner_from_message("ORB-11513", "Won't fix; infrastructure flake."),
        None
    );
    assert_eq!(
        covering_owner_from_message("ORB-11513", "Duplicate of active ORB-11513 itself."),
        None
    );
}
