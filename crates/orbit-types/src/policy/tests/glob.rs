use crate::policy::{GlobReach, PolicyError, compile_glob_regex, match_glob, normalize_glob_path};

#[test]
fn glob_matching_preserves_segment_aware_wildcards() {
    let path = normalize_glob_path("crates/orbit-engine/perf_runner.rs").expect("normalize");
    assert!(match_glob("**/perf*.rs", &path).expect("match glob"));

    let nested = normalize_glob_path("foo/bar/baz.rs").expect("normalize");
    assert!(!match_glob("foo/*.rs", &nested).expect("match glob"));
}

#[test]
fn normalization_canonicalizes_equivalent_paths() {
    for spelling in [
        "secret/./key.txt",
        "secret//key.txt",
        "secret/key.txt/",
        "././secret/key.txt",
    ] {
        let normalized = normalize_glob_path(spelling).expect("normalize");
        assert_eq!(normalized, "secret/key.txt");
        assert!(match_glob("secret/key.txt", &normalized).expect("match"));
    }
}

#[test]
fn normalization_rejects_workspace_escapes() {
    assert!(matches!(
        normalize_glob_path("../escape"),
        Err(PolicyError::Invalid(_))
    ));
}

#[test]
fn trailing_double_star_matches_subtree_and_anchor() {
    let nested = normalize_glob_path("foo/bar/baz.rs").expect("normalize");
    assert!(match_glob("foo/**", &nested).expect("match"));

    let root = normalize_glob_path("foo").expect("normalize");
    assert!(match_glob("foo/**", &root).expect("match"));
}

#[test]
fn wildcard_prefix_subtree_rules_match_and_reject_expected_paths() {
    for (rule, path) in [
        ("**/secrets/**", "secrets/key.txt"),
        ("**/secrets/**", "app/config/secrets/key.txt"),
        ("**/.git/**", "vendor/dep/.git/HEAD"),
        ("*/dir/**", "top/dir/nested/file.rs"),
    ] {
        let normalized = normalize_glob_path(path).expect("normalize");
        assert!(match_glob(rule, &normalized).expect("match glob"));
    }

    for (rule, path) in [
        ("*/dir/**", "a/b/dir/file.rs"),
        ("**/secrets/**", "my-secrets-backup/key.txt"),
        ("**/.git/**", "not-a.git/file"),
    ] {
        let normalized = normalize_glob_path(path).expect("normalize");
        assert!(!match_glob(rule, &normalized).expect("match glob"));
    }
}

#[test]
fn wildcard_prefix_subtree_rule_compiles_and_matches() {
    let regex = compile_glob_regex("**/secrets/**").expect("compile valid glob");
    let path = normalize_glob_path("app/secrets/private.key").expect("normalize");
    assert!(regex.is_match(&path));
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn globs_match_case_variants_on_case_insensitive_platforms() {
    let path = normalize_glob_path("config/Secret.ENV").expect("normalize");
    assert!(match_glob("**/*.env", &path).expect("match glob"));
}

#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn globs_remain_case_sensitive_on_case_sensitive_platforms() {
    let path = normalize_glob_path("Secret.ENV").expect("normalize");
    assert!(!match_glob("**/*.env", &path).expect("match glob"));
}

/// A rule that names a path beneath the root has to say so before that path
/// exists — the caller compiling it into a kernel ruleset cannot re-ask later.
#[test]
fn a_rule_names_a_path_beneath_a_directory_it_can_still_reach() {
    for (rule, dir) in [
        (".env", ""),
        ("secrets/**", ""),
        ("secrets/**", "secrets"),
        ("secrets/**", "secrets/nested"),
        ("config/*.key", ""),
        ("config/*.key", "config"),
        ("build/*/out.log", "build"),
        ("build/*/out.log", "build/debug"),
    ] {
        let reach = GlobReach::compile(rule).expect("compile reach");
        assert!(
            reach.names_beneath(dir),
            "`{rule}` can name a path beneath `{dir}`"
        );
    }
}

#[test]
fn a_rule_names_nothing_beneath_a_directory_it_cannot_reach() {
    for (rule, dir) in [
        (".env", "src"),
        ("secrets/**", "src"),
        ("config/*.key", "src"),
        ("config/*.key", "config/nested"),
        ("build/*/out.log", "build/debug/deps"),
        // `.` is the workspace root itself, not anything inside it.
        (".", ""),
    ] {
        let reach = GlobReach::compile(rule).expect("compile reach");
        assert!(
            !reach.names_beneath(dir),
            "`{rule}` cannot name a path beneath `{dir}`"
        );
    }
}

/// The `./a/b` and `a/b` spellings of a directory are the same directory.
#[test]
fn reach_accepts_either_spelling_of_the_workspace_root() {
    let reach = GlobReach::compile("secrets/**").expect("compile reach");

    assert!(reach.names_beneath("."));
    assert!(reach.names_beneath(""));
}

/// The distinction a caller with a per-directory cost needs: a rule whose own
/// segments bound it can be carved out ahead of time, and one whose `**`
/// crosses directories reaches everywhere, including directories that do not
/// exist yet.
#[test]
fn only_a_directory_crossing_wildcard_leaves_the_reach_unbounded() {
    for bounded in [".env", "secrets/**", "config/*.key", "**", "a/b/c"] {
        assert!(
            GlobReach::compile(bounded)
                .expect("compile reach")
                .is_bounded(),
            "`{bounded}` is bounded by its own segments"
        );
    }

    for unbounded in ["**/.env", "**/*.env", "**/secrets/**", "a/**/b.key"] {
        assert!(
            !GlobReach::compile(unbounded)
                .expect("compile reach")
                .is_bounded(),
            "`{unbounded}` can name a path beneath a directory it never mentions"
        );
    }
}

/// A trailing `**` covers every depth beneath its prefix, so every directory
/// inside that subtree can still gain a denied name.
#[test]
fn a_trailing_subtree_wildcard_reaches_every_depth_beneath_its_prefix() {
    let reach = GlobReach::compile("secrets/**").expect("compile reach");

    for dir in ["secrets", "secrets/a", "secrets/a/b/c"] {
        assert!(reach.names_beneath(dir), "beneath `{dir}`");
    }
}
