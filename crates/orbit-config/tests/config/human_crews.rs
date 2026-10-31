// `[crews.<name>] kind = "human"`: an operator crew that loads apart from the
// agent registry and that no pool, default or review crew may name.

/// Load a global `config.toml` holding `body` under an empty workspace.
fn human_crew_load(body: &str) -> Result<ResolvedConfig, String> {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    fs::write(global.path().join("config.toml"), body).expect("global config");
    ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .map_err(|error| error.to_string())
}

const HUMAN_CREW_AGENTS: &str = "[crews.sol]\nmodel = \"m\"\nprovider = \"codex\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";

#[test]
fn human_crew_loads_apart_from_the_agent_registry() {
    let config = human_crew_load(&format!(
        "[workflow]\ndefault_crew = \"sol\"\n\n{HUMAN_CREW_AGENTS}"
    ))
    .expect("a human crew with only description and tags loads");
    let human = config.human_crews.get("dk").expect("human crew registered");
    assert_eq!(human.description.as_deref(), Some("Daniel's own work"));
    assert!(
        !config.crews.contains_key("dk"),
        "a human crew must never enter the agent registry pools and defaults resolve against"
    );
    assert!(config.crews.contains_key("sol"));

    // Seeding a human crew into a document with no agent crews must not empty
    // the agent registry: the built-in crews still apply.
    let only_human = human_crew_load("[crews.dk]\nkind = \"human\"\n").expect("human-only crews");
    assert!(only_human.human_crews.contains_key("dk"));
    assert!(
        only_human.crews.contains_key("opus"),
        "{:?}",
        only_human.crews.keys()
    );
}

#[test]
fn human_crew_refuses_agent_properties() {
    for (property, line) in [
        ("provider", "provider = \"claude\""),
        ("model", "model = \"opus\""),
        ("effort", "effort = \"high\""),
        ("env", "env = [\"TOKEN\"]"),
    ] {
        let error = human_crew_load(&format!("[crews.dk]\nkind = \"human\"\n{line}\n"))
            .expect_err(property);
        assert!(
            error.contains("[crews.dk] is a human crew"),
            "{property}: {error}"
        );
        assert!(error.contains(property), "{property}: {error}");
    }
    let error = human_crew_load("[crews.dk]\nkind = \"robot\"\n").expect_err("unknown kind");
    assert!(error.contains("kind"), "{error}");
}

#[test]
fn human_crew_is_refused_wherever_an_agent_is_drawn() {
    for (key, setting) in [
        (
            "workflow.default_crew",
            "[workflow]\ndefault_crew = \"dk\"\n",
        ),
        (
            "workflow.low_complexity_crews",
            "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\", \"dk\"]\n",
        ),
        (
            "workflow.purpose_crews.task_pilot",
            "[workflow]\ndefault_crew = \"sol\"\n\n[workflow.purpose_crews]\ntask_pilot = [\"dk\"]\n",
        ),
        (
            "workflow.purpose_crews.review",
            "[workflow]\ndefault_crew = \"sol\"\n\n[workflow.purpose_crews]\nreview = \"dk\"\n",
        ),
        (
            "workflow.type_complexity_crews.ui.low",
            "[workflow]\ndefault_crew = \"sol\"\n\n[[workflow.crew_performance_types]]\nname = \"ui\"\n\
             tags = [\"ui\"]\n\n[workflow.type_complexity_crews.ui]\nlow = [\"dk\"]\n",
        ),
    ] {
        let error = human_crew_load(&format!("{setting}\n{HUMAN_CREW_AGENTS}")).expect_err(key);
        assert!(
            error.contains("which is a human crew"),
            "{key} must refuse the human crew by what it is: {error}"
        );
        assert!(error.contains("'dk'"), "{key}: {error}");
    }
}

#[test]
fn human_crew_name_cannot_shadow_an_agent_crew() {
    // The document declares no agent crews, so the built-in `opus` applies.
    let error = human_crew_load("[crews.opus]\nkind = \"human\"\n").expect_err("collision");
    assert!(error.contains("opus"), "{error}");
    assert!(error.contains("agent crew"), "{error}");
}
