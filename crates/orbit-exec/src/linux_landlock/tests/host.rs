use std::fs;
use std::path::Path;

use super::super::host::host_read_grants;
use crate::linux_landlock::grants_read;

fn environment(pairs: &[(&str, &Path)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, path)| (name.to_string(), path.display().to_string()))
        .collect()
}

/// The table's whole purpose is to be narrower than `$HOME`. A variable that
/// names the home directory itself, or an ancestor of it, is refused rather
/// than silently widening every grant.
#[test]
fn a_tool_state_variable_cannot_widen_the_grant_to_the_home_directory() {
    let home = tempfile::tempdir().expect("home");
    let secret = home.path().join(".ssh/id_ed25519");
    fs::create_dir_all(home.path().join(".ssh")).expect("mkdir .ssh");
    fs::write(&secret, "key").expect("write key");

    for named_home in [home.path(), Path::new("/")] {
        let grants = host_read_grants(&environment(&[
            ("HOME", home.path()),
            ("CARGO_HOME", named_home),
        ]));
        assert!(!grants_read(&grants, &secret), "{named_home:?}: {grants:?}");
    }
}
