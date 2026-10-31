use super::*;

/// A backlog task of a human crew is operator work: automatic and explicit
/// admission exclude it as `human_crew` even when the allowlist names that
/// crew, and readiness reports that reason rather than `crew_not_allowed`.
#[test]
fn a_human_crew_task_is_never_admitted_even_when_the_allowlist_names_it() {
    let config = r#"
[workflow]
default_crew = "sol"
[crews.sol]
provider = "codex"
model = "fixture-shared"
[crews.dk]
kind = "human"
"#;
    let (_root, runtime, _) = configured_runtime(config, &["src/fixture.rs", "src/other.rs"]);
    let add = |title: &str, crew: Option<&str>, file: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Human crew admission fixture".into(),
                acceptance_criteria: vec!["Never dispatched".into()],
                complexity: TaskComplexity::Low,
                context_files: vec![format!("file:{file}")],
                crew: crew.map(str::to_string),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    let human = add("Renew the domain", Some("dk"), "src/fixture.rs");
    let agent = add("Agent work", None, "src/other.rs");

    for input in [
        json!({}),
        json!({"allowed_crews": ["dk"]}),
        json!({"allowed_crews": ["dk", "sol"]}),
        json!({"task_ids": [human]}),
    ] {
        let output = list_backlog_tasks(&runtime, input.clone());
        assert!(!admitted(&output).contains(&human), "{input}: {output:#}");
        let entry = excluded_entry(&output, &human);
        assert_eq!(entry["reason"], "human_crew", "{input}: {entry}");
        assert_eq!(entry["crew"], "dk", "{input}: {entry}");
    }
    assert!(
        admitted(&list_backlog_tasks(
            &runtime,
            json!({"allowed_crews": ["dk", "sol"]})
        ))
        .contains(&agent),
        "an agent task beside it is still admitted"
    );

    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &["dk".to_string()])
        .expect("readiness with a human crew allowlist");
    let entry = readiness_task(&readiness, &human);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "human_crew", "{entry}");
    assert_eq!(entry["crew"], "dk", "{entry}");
}
