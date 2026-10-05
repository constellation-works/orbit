use serde_json::Value;

use super::super::activity_v2::{Provider, ProviderEntryPoint, ProviderResolveRequest};

const CONTRACT_JSON: &str = include_str!("fixtures/provider_contract.json");

/// Guards the Constellation provider-resolution conformance contract (§8)
/// across all vendored fixture cases (ORB-10091, ORB-14135).
///
/// Iterates every fixture case in `provider_contract.json` and asserts that
/// [`Provider::resolve`] produces the expected status, normalized provider,
/// precedence source, diagnostic code, and deprecation signal.
#[test]
fn provider_resolution_matches_vendored_conformance_cases() {
    let contract: Value =
        serde_json::from_str(CONTRACT_JSON).expect("parse pinned provider contract fixture");
    let cases = contract["cases"]
        .as_array()
        .expect("cases array in contract fixture");

    let mut asserted_cases = 0usize;

    for case in cases {
        let input = &case["input"];
        let id = case["id"].as_str().unwrap_or("<unknown>");

        let entry_points: &[ProviderEntryPoint] = match input["entry_point"]
            .as_str()
            .unwrap_or_else(|| panic!("entry_point must be a string in case {id}"))
        {
            "any" => &[
                ProviderEntryPoint::Orbit,
                ProviderEntryPoint::Worker,
                ProviderEntryPoint::Bridge,
            ],
            "orbit" => &[ProviderEntryPoint::Orbit],
            "worker" => &[ProviderEntryPoint::Worker],
            "bridge" => &[ProviderEntryPoint::Bridge],
            other => panic!("unexpected entry_point '{other}' in case {id}"),
        };

        for &entry_point in entry_points {
            let request = ProviderResolveRequest {
                entry_point,
                requested: input["requested"].as_str(),
                task_provider: input["task_provider"].as_str(),
                workspace_default: input["workspace_default"].as_str(),
                env_default: input["env_default"].as_str(),
                system_default: input["system_default"].as_str(),
                persisted_resolution: input["persisted_resolution"].as_str(),
                host_available: input["host_available"].as_bool(),
            };

            let got = Provider::resolve(&request);
            let expected = &case["expected"];

            assert_eq!(
                got.is_success(),
                expected["status"].as_str() == Some("success"),
                "status mismatch for case {id} at entry point {entry_point:?}",
            );

            assert_eq!(
                got.normalized_provider.map(Provider::as_str),
                expected["normalized_provider"].as_str(),
                "normalized_provider mismatch for case {id} at entry point {entry_point:?}",
            );

            assert_eq!(
                Some(got.source.as_str()),
                expected["source"].as_str(),
                "source mismatch for case {id} at entry point {entry_point:?}",
            );

            assert_eq!(
                Some(got.diagnostic.as_str()),
                expected["diagnostic"].as_str(),
                "diagnostic mismatch for case {id} at entry point {entry_point:?}",
            );

            match expected["deprecation"].as_object() {
                Some(obj) => {
                    let deprecation = got
                        .deprecation
                        .as_ref()
                        .unwrap_or_else(|| panic!("expected deprecation signal in case {id}"));
                    assert_eq!(
                        Some(deprecation.alias.as_str()),
                        obj["alias"].as_str(),
                        "deprecation.alias mismatch for case {id}",
                    );
                    assert_eq!(
                        Some(deprecation.canonical.as_str()),
                        obj["canonical"].as_str(),
                        "deprecation.canonical mismatch for case {id}",
                    );
                }
                None => {
                    assert!(
                        got.deprecation.is_none(),
                        "unexpected deprecation {:?} in case {id}",
                        got.deprecation,
                    );
                }
            }
        }

        asserted_cases += 1;
    }

    assert_eq!(
        asserted_cases,
        cases.len(),
        "all conformance fixture cases must be asserted",
    );
    assert_eq!(
        asserted_cases, 28,
        "conformance fixture must contain the expected 28 cases",
    );
}
