use super::super::version::{SemverRange, Version};

fn v(raw: &str) -> Version {
    raw.parse().expect("version")
}

#[test]
fn parses_versions_with_prerelease_and_build() {
    let version = v("1.2.3-rc.1+build");
    assert_eq!((version.major, version.minor, version.patch), (1, 2, 3));
    assert_eq!(version.pre, "rc.1");
    assert!("1.2".parse::<Version>().is_err());
    assert!("x.y.z".parse::<Version>().is_err());
}

#[test]
fn ranges_match_the_documented_operators() {
    let range = SemverRange::parse(">=0.24.0 <1.0.0").expect("range");
    assert!(range.matches(&v("0.24.0")));
    assert!(range.matches(&v("0.99.9")));
    assert!(!range.matches(&v("1.0.0")));
    assert!(!range.matches(&v("0.23.9")));

    let caret = SemverRange::parse("^1.2.0").expect("caret");
    assert!(caret.matches(&v("1.9.0")));
    assert!(!caret.matches(&v("2.0.0")));

    let zero_caret = SemverRange::parse("0.4.1").expect("bare");
    assert!(zero_caret.matches(&v("0.4.9")));
    assert!(!zero_caret.matches(&v("0.5.0")));

    let tilde = SemverRange::parse("~1.2.3").expect("tilde");
    assert!(tilde.matches(&v("1.2.9")));
    assert!(!tilde.matches(&v("1.3.0")));

    let x_range = SemverRange::parse("0.4.x").expect("x-range");
    assert!(x_range.matches(&v("0.4.0")));
    assert!(x_range.matches(&v("0.4.9")));
    assert!(!x_range.matches(&v("0.5.0")));

    let either = SemverRange::parse("=0.1.0 || >=2.0.0").expect("or");
    assert!(either.matches(&v("0.1.0")));
    assert!(either.matches(&v("2.1.0")));
    assert!(!either.matches(&v("1.0.0")));

    assert!(SemverRange::parse("*").expect("any").matches(&v("9.9.9")));
    assert!(SemverRange::parse(">= nope").is_err());
}

#[test]
fn prerelease_sorts_before_release() {
    let range = SemverRange::parse(">=1.0.0").expect("range");
    assert!(!range.matches(&v("1.0.0-beta")));
    assert!(range.matches(&v("1.0.0")));
}

#[test]
fn prerelease_precedence_and_range_matching_follow_semver() {
    assert!(v("1.0.0-rc.10") > v("1.0.0-rc.9"));

    let release_range = SemverRange::parse(">=0.24.0").expect("range");
    assert!(!release_range.matches(&v("0.25.0-alpha")));

    let prerelease_range = SemverRange::parse(">=0.25.0-alpha").expect("range");
    assert!(prerelease_range.matches(&v("0.25.0-beta")));
}
