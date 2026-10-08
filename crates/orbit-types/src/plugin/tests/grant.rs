use super::super::PluginGrantError;
use super::super::grant::{PluginGrant, parse_grants, parse_stored_grants};

/// A grant set `parse_grants` accepts must load back as the same set.
///
/// Joining bare `fs` roots verbatim recorded `fs=data,cache`, which fails to
/// parse, and `fs=./a,network`, which loads the `network` grant the operator
/// never gave. The recorded continuation is `./<root>`, and the set keeps the
/// bare root.
#[test]
fn a_recorded_grant_set_loads_as_the_same_set() {
    let accepted: &[&[&str]] = &[
        &["fs"],
        &["network", "env_pass"],
        &["fs=data"],
        &["fs=data", "fs=cache"],
        &["fs=./a", "fs=network"],
        &["fs=/srv/data,/srv/cache"],
        &["fs=./a,../b,~/c,{{workspace}}/d"],
        &["fs=data,./cache", "network"],
        &["fs=/tmp,./explicit"],
        &["fs=/tmp,network"],
        &["fs=foo=bar"],
        &["unsandboxed", "orbit_tools", "fs=./data,./cache"],
        &["fs=data", "fs=cache", "fs=logs"],
        &["fs=./cache", "fs=cache"],
    ];
    for input in accepted {
        let names = owned(input);
        let set = parse_grants(&names).unwrap_or_else(|error| {
            panic!("parse_grants({input:?}) should accept the list: {error}")
        });
        let recorded = set.to_recorded();
        let loaded = parse_stored_grants(&recorded).unwrap_or_else(|error| {
            panic!("recorded {recorded:?} from {input:?} should parse: {error}")
        });
        assert_eq!(
            loaded, set,
            "parse_stored_grants(to_recorded()) must equal the accepted set for {input:?}; recorded {recorded:?}"
        );
    }

    let merged = parse_grants(&owned(&["fs=data", "fs=cache"])).expect("two bare fs roots");
    assert_eq!(
        merged.fs_roots().map(|roots| roots.to_vec()),
        Some(vec!["data".to_string(), "cache".to_string()]),
        "merged bare roots stay data and cache"
    );
    assert_eq!(merged.grants(), vec![PluginGrant::Fs]);
    assert_eq!(
        merged.to_recorded(),
        vec!["fs=data,./cache".to_string()],
        "the stored row must keep cache from being read as a grant name"
    );

    let disguised = parse_grants(&owned(&["fs=./a", "fs=network"])).expect("root named network");
    assert_eq!(
        disguised.fs_roots().map(|roots| roots.to_vec()),
        Some(vec!["./a".to_string(), "network".to_string()])
    );
    assert!(
        !disguised.contains(PluginGrant::Network),
        "a root named network must not become the network grant: {}",
        disguised.to_recorded().join(", ")
    );

    // A continuation that does not look like a path is still a typo, not a
    // root. The `./` prefix is how an operator names a bare continuation in
    // one comma-separated value.
    let typo = parse_grants(&owned(&["fs=/srv/data,netwrok"])).expect_err("typo is not a root");
    assert_eq!(
        typo,
        PluginGrantError::Unknown {
            names: vec!["netwrok".to_string()]
        },
        "a continuation that does not look like a path stays an unknown grant"
    );
    let bare_in_one_value =
        parse_grants(&owned(&["fs=data,cache"])).expect_err("bare continuation needs ./");
    assert_eq!(
        bare_in_one_value,
        PluginGrantError::Unknown {
            names: vec!["cache".to_string()]
        },
        "one comma list still requires the continuation to look like a path"
    );

    // `network` after a comma, with no path prefix, is the grant name. That
    // spelling is how an operator grants both in one value.
    let typed_grant = parse_grants(&owned(&["fs=/tmp,network"])).expect("comma then a grant name");
    assert!(typed_grant.contains(PluginGrant::Fs));
    assert!(typed_grant.contains(PluginGrant::Network));
    assert_eq!(
        typed_grant.fs_roots().map(|roots| roots.to_vec()),
        Some(vec!["/tmp".to_string()])
    );
}

fn owned(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}
