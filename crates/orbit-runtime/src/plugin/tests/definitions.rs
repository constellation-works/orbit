//! Provenance reads stay inside the definitions directory they were given.
//! A traversal path or a symlink that leaves that directory must not be opened.

use std::fs;
use std::path::Path;

use super::super::definitions::read_definition_provenance;

fn write_provenance(path: &Path, namespace: &str, version: &str) {
    fs::write(
        path,
        format!("# provenance: plugin:{namespace}@{version}\nname: {namespace}\n"),
    )
    .expect("write definition");
}

#[test]
fn provenance_read_rejects_paths_that_leave_the_definition_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let definitions = root.path().join("routines");
    fs::create_dir(&definitions).expect("definitions directory");
    write_provenance(&definitions.join("demo.yaml"), "demo", "1.2.3");
    let local = definitions.join("local");
    fs::create_dir(&local).expect("local definitions");
    write_provenance(&local.join("nested.yaml"), "nested", "2.0.0");

    let outside = root.path().join("outside.yaml");
    write_provenance(&outside, "escaped", "9.9.9");

    assert_eq!(
        read_definition_provenance(&definitions, &definitions.join("demo.yaml")),
        Some(("demo".to_string(), "1.2.3".to_string())),
        "a regular seeded file inside the definitions directory keeps its provenance"
    );
    assert_eq!(
        read_definition_provenance(&definitions, &local.join("nested.yaml")),
        Some(("nested".to_string(), "2.0.0".to_string())),
        "a regular file under routines/local stays readable"
    );
    assert_eq!(
        read_definition_provenance(&definitions, &definitions.join("..").join("outside.yaml")),
        None,
        "a traversal path must not open a file outside the definitions directory"
    );
    assert_eq!(
        read_definition_provenance(&definitions, &outside),
        None,
        "an absolute path outside the definitions directory must not be opened"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        symlink(&outside, definitions.join("linked.yaml")).expect("file symlink");
        assert_eq!(
            read_definition_provenance(&definitions, &definitions.join("linked.yaml")),
            None,
            "a symlink to a file outside the definitions directory must not be followed"
        );

        let elsewhere = tempfile::tempdir().expect("symlink target");
        let escaped_dir = elsewhere.path().join("routines");
        fs::create_dir(&escaped_dir).expect("escaped definitions");
        write_provenance(&escaped_dir.join("stolen.yaml"), "stolen", "0.0.1");
        let linked_root = root.path().join("linked-routines");
        symlink(&escaped_dir, &linked_root).expect("directory symlink");
        assert_eq!(
            read_definition_provenance(&linked_root, &linked_root.join("stolen.yaml")),
            None,
            "a symlinked definitions directory must not become the permitted root"
        );
    }
}
