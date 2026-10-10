use std::fs;

use tempfile::tempdir;

use super::super::operator_env::{parse_env_text, read_env_file};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn only_pass_listed_non_empty_names_are_taken_from_the_text() {
    let text = "\
# comment
export CLAUDE_CODE_OAUTH_TOKEN=\"quoted token\"
UNLISTED_SECRET=nope
ORBIT_OPERATOR=1
EMPTY=
 SINGLE = 'single quoted'
DUP=first
DUP=second
no equals sign
";
    let pass = names(&[
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ORBIT_OPERATOR",
        "EMPTY",
        "SINGLE",
        "DUP",
    ]);
    assert_eq!(
        parse_env_text(text, &pass),
        [
            (
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "quoted token".to_string()
            ),
            ("SINGLE".to_string(), "single quoted".to_string()),
            ("DUP".to_string(), "second".to_string()),
        ],
        "unlisted, ORBIT_ and empty entries are dropped; the last duplicate wins"
    );
}

#[cfg(unix)]
#[test]
fn a_file_other_users_can_read_is_refused_and_an_owner_only_one_is_read() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("clock.env");
    let pass = names(&["TOKEN"]);
    assert!(
        read_env_file(&path, &pass).unwrap().is_none(),
        "absent file"
    );

    fs::write(&path, "TOKEN=abc\n").unwrap();
    for mode in [0o644, 0o640, 0o604] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let error = read_env_file(&path, &pass).unwrap_err().to_string();
        assert!(
            error.contains("chmod 600") && !error.contains("abc"),
            "mode {mode:o} must be refused without echoing the value: {error}"
        );
    }

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_env_file(&path, &pass).unwrap().unwrap(),
        [("TOKEN".to_string(), "abc".to_string())]
    );

    let link = dir.path().join("link.env");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(
        read_env_file(&link, &pass).is_err(),
        "a symlinked env file is refused"
    );
}
