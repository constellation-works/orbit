//! Provider launcher lookup refuses parent-directory traversal in the
//! configured program while explicit and bare launchers still resolve.

use tempfile::tempdir;

use super::super::launcher::{locate_provider_launcher, resolve_provider_launcher};
use super::test_support::write_executable;

#[test]
fn launcher_lookup_refuses_parent_directory_traversal() {
    let root = tempdir().expect("tempdir");
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).expect("bin dir");
    let launcher = bin.join("fake-agent");
    write_executable(&launcher, "#!/bin/sh\n");

    let explicit = launcher.to_string_lossy().into_owned();
    assert_eq!(
        locate_provider_launcher(&explicit, None),
        Some(launcher.clone()),
        "an explicit launcher path still resolves"
    );
    assert_eq!(
        locate_provider_launcher("bin/fake-agent", Some(root.path())),
        Some(launcher.clone()),
        "a relative explicit launcher still resolves against the dispatch cwd"
    );
    let traversing_cwd = bin.join("..");
    assert_eq!(
        locate_provider_launcher("bin/fake-agent", Some(&traversing_cwd)),
        None,
        "the final lookup must refuse traversal supplied by cwd"
    );
    assert_eq!(
        locate_provider_launcher(&explicit, Some(&traversing_cwd)),
        Some(launcher.clone()),
        "an absolute launcher does not depend on cwd"
    );

    let traversal = bin.join("..").join("bin").join("fake-agent");
    let traversal = traversal.to_string_lossy().into_owned();
    for program in [traversal.as_str(), "..", "../bin/fake-agent"] {
        assert_eq!(
            locate_provider_launcher(program, Some(&bin)),
            None,
            "lookup must not resolve {program:?}"
        );
        let error = resolve_provider_launcher("codex", program, Some(&bin))
            .expect_err("dispatch must refuse a traversing launcher");
        assert!(
            error.permanent,
            "a traversing launcher is a permanent configuration error"
        );
    }
}
