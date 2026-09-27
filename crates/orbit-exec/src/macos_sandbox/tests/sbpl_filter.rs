use super::super::test_support::*;

#[test]
fn compile_strips_glob_suffix_for_subpath_root() {
    let resolved = profile(
        "default",
        &["/Users/test/repo"],
        &["/Users/test/repo/src/**"],
    );
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(
        text.contains("(allow file-write* (subpath \"/Users/test/repo/src\"))"),
        "expected glob-stripped subpath: {text}"
    );
    assert!(
        !text.contains("/src/**"),
        "subpath should not contain glob marker: {text}"
    );
}

#[test]
fn compile_uses_regex_for_non_subpath_positive_modify_glob() {
    let resolved = profile(
        "default",
        &["/Users/test/repo"],
        &["/Users/test/.orbit/orbit.db*"],
    );
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(
        text.contains(
            "(allow file-write* (regex \"^/[Uu][Ss][Ee][Rr][Ss]/[Tt][Ee][Ss][Tt]/\\\\.[Oo][Rr][Bb][Ii][Tt]/[Oo][Rr][Bb][Ii][Tt]\\\\.[Dd][Bb][^/]*$\"))"
        ),
        "missing regex allow for SQLite sidecar glob: {text}"
    );
    // SBPL has no `(?i)` inline flag — case-insensitivity is expressed via
    // per-letter character classes (ORB-00372). A stray `(?i)` makes
    // sandbox-exec reject the whole profile.
    assert!(
        !text.contains("(?i)"),
        "emitted SBPL must not contain the unsupported (?i) inline flag: {text}"
    );
    assert!(
        !text.contains("(allow file-write* (subpath \"/Users/test/.orbit\"))"),
        "positive file glob must not collapse to the whole Orbit root: {text}"
    );
}

#[test]
fn compile_appends_explicit_deny_for_negated_modify_rule() {
    let mut resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo"]);
    resolved.modify.push("!/Users/test/repo/.env".to_string());
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(
        text.contains("(deny file-write* (subpath \"/Users/test/repo/.env\"))"),
        "missing deny clause: {text}"
    );
    let allow_pos = text
        .find("(allow file-write* (subpath \"/Users/test/repo\"))")
        .expect("allow clause present");
    let deny_pos = text
        .find("(deny file-write* (subpath \"/Users/test/repo/.env\"))")
        .expect("deny clause present");
    assert!(
        allow_pos < deny_pos,
        "deny clause must come after allow for last-match-wins: {text}"
    );
}

#[test]
fn compile_emits_explicit_read_deny_for_negated_read_rule() {
    // Invariant: `denyRead` rules (negated entries in `read`) must
    // translate to explicit `(deny file-read* ...)` clauses appended
    // after the broad `(allow file-read*)` so they win under
    // last-match-wins. This is the kernel-side complement to
    // `compile_appends_explicit_deny_for_negated_modify_rule`.
    let mut resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo"]);
    resolved.read.push("!/Users/test/repo/.env".to_string());
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(
        text.contains("(deny file-read* (subpath \"/Users/test/repo/.env\"))"),
        "missing deny file-read* clause: {text}"
    );
    let allow_pos = text.find("(allow file-read*)").expect("broad read allow");
    let deny_pos = text
        .find("(deny file-read* (subpath \"/Users/test/repo/.env\"))")
        .expect("read deny clause");
    assert!(
        allow_pos < deny_pos,
        "deny file-read* must come after broad allow for last-match-wins: {text}"
    );
}

#[test]
fn compile_uses_regex_for_non_subpath_negated_read_glob() {
    // Invariant: a `denyRead` rule with a non-trivial glob (e.g.
    // `!**/secrets/**`) must compile to a regex deny clause, not a
    // collapsed subpath that would over-match.
    let mut resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo"]);
    resolved.read.push("!/Users/test/repo/**/*.env".to_string());
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(
        text.contains("(deny file-read* (regex \"^/[Uu][Ss][Ee][Rr][Ss]/[Tt][Ee][Ss][Tt]/[Rr][Ee][Pp][Oo]/([^/]+/)*[^/]*\\\\.[Ee][Nn][Vv]$\"))"),
        "missing regex read deny: {text}"
    );
    assert!(
        !text.contains("(?i)"),
        "emitted SBPL must not contain the unsupported (?i) inline flag: {text}"
    );
}

#[test]
fn compile_uses_regex_for_non_subpath_negated_modify_glob() {
    let mut resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo"]);
    resolved
        .modify
        .push("!/Users/test/repo/**/*.env".to_string());
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(
        text.contains(
            "(deny file-write* (regex \"^/[Uu][Ss][Ee][Rr][Ss]/[Tt][Ee][Ss][Tt]/[Rr][Ee][Pp][Oo]/([^/]+/)*[^/]*\\\\.[Ee][Nn][Vv]$\"))"
        ),
        "missing regex deny for env glob: {text}"
    );
    assert!(
        !text.contains("(?i)"),
        "emitted SBPL must not contain the unsupported (?i) inline flag: {text}"
    );
    assert!(
        !text.contains("(deny file-write* (subpath \"/Users/test/repo\"))"),
        "env glob must not collapse to a repo-wide deny: {text}"
    );
}

#[test]
fn regex_deny_filters_match_case_variant_secret_paths() {
    let env_regex = regex_from_filter(&super::super::sbpl_filter::sbpl_filter_for_deny_rule(
        "/Users/test/repo/**/*.env",
    ));
    assert!(
        env_regex.is_match("/Users/test/repo/Secret.ENV"),
        "env deny regex should match case-varied root dotenv paths"
    );
    assert!(
        env_regex.is_match("/Users/test/repo/config/Secret.ENV"),
        "env deny regex should match case-varied nested dotenv paths"
    );

    let orbit_regex = regex_from_filter(&super::super::sbpl_filter::sbpl_filter_for_deny_rule(
        "/Users/test/repo/**/.orbit/**",
    ));
    assert!(
        orbit_regex.is_match("/Users/test/repo/.Orbit/state/task.json"),
        "orbit deny regex should match case-varied .orbit paths"
    );
}

#[cfg(unix)]
#[test]
fn compiled_read_glob_uses_the_physical_prefix_and_covers_future_files() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    std::fs::create_dir(&real).expect("real directory");
    let alias = temp.path().join("alias");
    symlink(&real, &alias).expect("path alias");
    let future = real.join("later/sub/.env");
    assert!(
        !future.exists(),
        "the denied file must be absent at compile time"
    );

    let mut resolved = profile("default", &["/"], &[]);
    resolved.read.push(format!("!{}/**/.env", alias.display()));
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    let regex = compiled_read_deny_regex(&text);
    std::fs::create_dir_all(future.parent().expect("future parent")).expect("create parent");
    std::fs::write(&future, "secret").expect("create file after compile");

    assert!(
        regex.is_match(&future.display().to_string()),
        "the emitted deny must cover a future file under the physical path: {text}"
    );
    assert!(
        !regex.is_match(&real.join("later/sub/public.txt").display().to_string()),
        "the emitted deny must preserve the interior wildcard's narrow scope: {text}"
    );
}

#[cfg(unix)]
#[test]
fn compiled_default_credential_deny_and_keychain_reallow_share_physical_home() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    std::fs::create_dir(&real).expect("real directory");
    let alias = temp.path().join("alias");
    symlink(&real, &alias).expect("path alias");
    let home = alias.join("home");
    let physical_home = real.join("home");
    let home_text = home.display().to_string();
    let text = compile_with_env(
        &profile("default", &["/"], &[]),
        "claude",
        EnvOverrides {
            home: Some(&home_text),
            ..EnvOverrides::default()
        },
    );

    let deny = compiled_subpath(&text, "deny", "Library/Keychains");
    let reallow = compiled_subpath(&text, "allow", "Library/Keychains");
    let keychains = physical_home.join("Library/Keychains/login.keychain-db");
    assert!(
        keychains.starts_with(&deny),
        "default deny must bind to the physical keychain: {text}"
    );
    assert!(
        keychains.starts_with(&reallow),
        "provider re-allow must bind to the same physical keychain: {text}"
    );
    assert_eq!(
        deny, reallow,
        "the deny and re-allow must describe one directory"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_read_glob_denies_private_var_alias_even_for_future_files() {
    let temp = tempfile::tempdir_in("/var/tmp").expect("tempdir under /var/tmp");
    let physical = temp.path().canonicalize().expect("physical tempdir");
    assert!(physical.starts_with("/private/var/tmp"));
    let future = physical.join("later/.env");
    assert!(
        !future.exists(),
        "the denied file must be absent at compile time"
    );

    let mut resolved = profile("default", &["/"], &[]);
    resolved
        .read
        .push(format!("!{}/**/.env", temp.path().display()));
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    let regex = compiled_read_deny_regex(&text);
    std::fs::create_dir_all(future.parent().expect("future parent")).expect("create parent");
    std::fs::write(&future, "secret").expect("create file after compile");

    assert!(
        regex.is_match(&future.display().to_string()),
        "the /var rule must deny the canonical /private/var file: {text}"
    );

    if sandbox_exec_can_apply() {
        let public = physical.join("later/public.txt");
        std::fs::write(&public, "public").expect("public file");
        assert!(
            can_read_under_profile(&text, &public),
            "the sibling should remain readable under sandbox-exec: {text}"
        );
        assert!(
            !can_read_under_profile(&text, &future),
            "sandbox-exec must deny the nested file created after compile: {text}"
        );
    }
}

#[cfg(unix)]
fn compiled_read_deny_regex(text: &str) -> regex::Regex {
    let filter = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("(deny file-read* ")
                .filter(|filter| filter.starts_with("(regex "))
        })
        .and_then(|line| line.strip_suffix(')'))
        .expect("compiled read deny");
    regex_from_filter(filter)
}

#[cfg(unix)]
fn compiled_subpath(text: &str, action: &str, suffix: &str) -> std::path::PathBuf {
    let prefix = format!("({action} file-read* (subpath \"");
    text.lines()
        .filter_map(|line| line.strip_prefix(&prefix)?.strip_suffix("\"))"))
        .map(std::path::PathBuf::from)
        .find(|path| path.ends_with(suffix))
        .expect("compiled credential subpath")
}

fn regex_from_filter(filter: &str) -> regex::Regex {
    let prefix = "(regex \"";
    let suffix = "\")";
    let escaped_regex = filter
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .expect("regex filter shape");
    let regex = escaped_regex.replace("\\\\", "\\").replace("\\\"", "\"");
    regex::Regex::new(&regex).expect("valid emitted regex")
}
