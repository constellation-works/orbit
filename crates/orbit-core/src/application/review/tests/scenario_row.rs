use git2::Repository;
use tempfile::TempDir;

use crate::application::review::scenario_row::ScenarioRows;

/// Index a `tests/<target>` directory holding `files` (name, source) through
/// the same `scan_target` seam the review gate calls.
fn scan_target(files: &[(&str, &str)]) -> ScenarioRows {
    let dir = TempDir::new().expect("temporary repository directory");
    let repo = Repository::init(dir.path()).expect("initialise repository");
    let mut builder = repo.treebuilder(None).expect("tree builder");
    for &(name, text) in files {
        let blob = repo.blob(text.as_bytes()).expect("write blob");
        builder.insert(name, blob, 0o100644).expect("insert file");
    }
    let tree = repo
        .find_tree(builder.write().expect("write tree"))
        .expect("read tree");
    let mut rows = ScenarioRows::default();
    rows.scan_target(&repo, &tree, "orbit-core", "regression");
    rows
}

#[test]
fn unclosed_attribute_at_target_end_does_not_panic_and_earlier_declarations_still_index() {
    // ORB-29641: an unclosed `#[` at the end of the root file reversed the
    // attribute slice bounds and panicked inside the review gate. The complete
    // `#[path]` declaration and the plain `mod` before it must still index.
    let rows = scan_target(&[
        (
            "main.rs",
            "#[path = \"nested.rs\"]\nmod mounted;\nmod plain;\n#[",
        ),
        ("nested.rs", "scenario_table!(mounted, mounted_row);"),
        ("plain.rs", "scenario_table!(plain, plain_row);"),
    ]);
    assert!(
        rows.is_row("plain_row"),
        "a plain module declared before an unclosed attribute must still index its rows"
    );
    assert!(
        rows.is_row("mounted_row"),
        "a complete #[path] module must still index its rows"
    );
}
