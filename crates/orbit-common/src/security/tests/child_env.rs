use super::super::child_env::allowlisted_child_env_from;

fn names(env: &[(String, String)]) -> Vec<&str> {
    env.iter().map(|(name, _)| name.as_str()).collect()
}

#[test]
fn an_explicit_pass_list_cannot_smuggle_a_privilege_bearing_orbit_name() {
    let parent = [
        ("ORBIT_OPERATOR", "1"),
        ("ORBIT_WORKSPACE_CLAIM_TOKEN", "abc123"),
        ("ORBIT_RUN_ID", "r1"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect::<Vec<_>>();
    let pass = vec![
        "ORBIT_OPERATOR".to_string(),
        "ORBIT_WORKSPACE_CLAIM_TOKEN".to_string(),
    ];

    let env = allowlisted_child_env_from(&parent, &pass, &["ORBIT_OPERATOR"]);

    // Named in `pass` and `extras` alike; still refused. Only the named
    // envelope variable, admitted on its own terms, survives.
    let orbit_names: Vec<&str> = names(&env)
        .into_iter()
        .filter(|name| name.starts_with("ORBIT_"))
        .collect();
    assert_eq!(
        orbit_names,
        ["ORBIT_RUN_ID"],
        "a privilege-bearing name must not ride an explicit pass/extras list either"
    );
}
