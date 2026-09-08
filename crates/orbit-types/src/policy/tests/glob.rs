use crate::policy::{PolicyError, compile_glob_regex, match_glob, normalize_glob_path};

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
