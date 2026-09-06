use crate::update::version::ReleaseVersion;

fn parse(value: &str) -> ReleaseVersion {
    ReleaseVersion::parse(value).expect("parse version")
}

#[test]
fn parses_bare_and_tagged_versions_identically() {
    assert_eq!(parse("0.18.0"), parse("v0.18.0"));
    assert_eq!(parse("v0.18.0").to_string(), "0.18.0");
    assert_eq!(parse("0.18.0").tag(), "v0.18.0");
}

#[test]
fn orders_by_numeric_component_not_lexically() {
    assert!(parse("0.9.0") < parse("0.10.0"));
    assert!(parse("1.0.0") > parse("0.99.99"));
    assert!(parse("0.18.1") > parse("0.18.0"));
}

#[test]
fn a_prerelease_sorts_below_the_release_it_leads_to() {
    assert!(parse("0.19.0-rc.1") < parse("0.19.0"));
    assert!(parse("0.19.0-rc.1") < parse("0.19.0-rc.2"));
    assert!(parse("0.19.0-rc.1") > parse("0.18.9"));
    assert_eq!(parse("0.19.0-rc.1").to_string(), "0.19.0-rc.1");
}

#[test]
fn prerelease_numbers_compare_numerically_not_lexically() {
    assert!(parse("0.19.0-rc.2") < parse("0.19.0-rc.10"));
    assert!(parse("0.19.0-rc.10") < parse("0.19.0"));
    assert!(parse("0.19.0-rc.10") > parse("0.18.9"));
    assert_eq!(parse("0.19.0-rc.10").to_string(), "0.19.0-rc.10");
}

#[test]
fn rejects_input_that_is_not_a_release_version() {
    for value in ["", "latest", "0.18", "0.18.0.1", "0.18.x", "v"] {
        let error = ReleaseVersion::parse(value).expect_err("should reject");
        assert!(
            error.to_string().contains("MAJOR.MINOR.PATCH"),
            "{value}: {error}"
        );
    }
}

#[test]
fn rejects_malformed_or_unsupported_prerelease_identifiers() {
    for value in [
        "0.19.0-",
        "0.19.0-rc.",
        "0.19.0-rc..1",
        "0.19.0-rc.01",
        "0.19.0-rc.1+build",
        "0.19.0-rc_1",
        "0.19.0-rc.1_2",
    ] {
        let error = ReleaseVersion::parse(value).expect_err("should reject");
        assert!(
            error.to_string().contains("MAJOR.MINOR.PATCH"),
            "{value}: {error}"
        );
    }
}
