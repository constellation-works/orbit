//  `[crews.<name>] env` routes host variables into one crew's agents, so only
//  the global config may set it: a workspace file that does fails the load
//  naming the crew, `config set` refuses it at workspace scope, and an entry
//  that is not `NAME` / `NAME=SOURCE` over non-`ORBIT_*` variable names fails
//  the load naming the entry.

const CREW_ENV_CREW: &str = "[workflow]\ndefault_crew = \"opus-credits\"\n\n\
     [crews.opus-credits]\nprovider = \"claude\"\nmodel = \"claude-opus-5-5\"\n";

fn crew_env_write_config(root: &Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

#[test]
fn crew_env_is_global_only_and_refused_from_a_workspace_file_naming_the_crew() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let env_line = "env = [\"ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1\"]\n";

    crew_env_write_config(global.path(), &format!("{CREW_ENV_CREW}{env_line}"));
    crew_env_write_config(workspace.path(), "[workflow]\nauto_ship = true\n");
    let loaded = ResolvedConfig::load(&roots).expect("a global crew env loads");
    let entries: Vec<String> = loaded.crews["opus-credits"]
        .env
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(entries, ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]);

    // The workspace layer may not add an env list, even to a crew the global
    // file defines.
    crew_env_write_config(
        workspace.path(),
        &format!("[crews.opus-credits]\n{env_line}"),
    );
    let refusal = ResolvedConfig::load(&roots)
        .expect_err("a workspace crew env must fail the load")
        .to_string();
    assert!(
        refusal.contains("[crews.opus-credits].env") && refusal.contains("global"),
        "the refusal names the crew and where the key belongs: {refusal}"
    );

    // `orbit config set` at workspace scope refuses before touching the file.
    let workspace_path = workspace.path().join("config.toml");
    crew_env_write_config(workspace.path(), "[workflow]\nauto_ship = true\n");
    let before = std::fs::read(&workspace_path).expect("read workspace config");
    let mut store = ConfigStore::open_for_workspace_set(
        &workspace_path,
        &global.path().join("config.toml"),
        WorkspaceInitMode::RequireExisting,
    )
    .expect("open workspace config");
    let refusal = store
        .set_value("crews.opus-credits.env", "[\"ANTHROPIC_API_KEY\"]")
        .expect_err("config set must refuse a workspace crew env")
        .to_string();
    assert!(refusal.contains("--global"), "{refusal}");
    assert_eq!(
        std::fs::read(&workspace_path).expect("reread workspace config"),
        before,
        "the refused write leaves the workspace file unchanged"
    );
}

#[test]
fn crew_env_entries_that_are_not_variable_names_or_name_orbit_variables_fail_naming_the_entry() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    for entry in [
        "1API_KEY",
        "ANTHROPIC-API-KEY",
        "ANTHROPIC_API_KEY=",
        "ANTHROPIC_API_KEY=CREDITS=1",
        "ORBIT_OPERATOR",
        "ANTHROPIC_API_KEY=ORBIT_WORKSPACE_CLAIM_TOKEN",
        "ORBIT_RUN_ID=ANTHROPIC_CREDITS_API_KEY_1",
    ] {
        crew_env_write_config(
            global.path(),
            &format!("{CREW_ENV_CREW}env = [\"{entry}\"]\n"),
        );
        let refusal = ResolvedConfig::load(&roots)
            .expect_err("an invalid crew env entry must fail the load")
            .to_string();
        assert!(
            refusal.contains("[crews.opus-credits].env") && refusal.contains(entry),
            "the refusal names the crew and the entry `{entry}`: {refusal}"
        );
    }
}
