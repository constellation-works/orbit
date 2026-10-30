//  `lease.intake.peer_user` and `lease.listener.*` are global-only: a
//  workspace file carrying either must fail ordinary layered loading,
//  effective-config reads and staged validation, naming the setting and
//  file, while the same values in the global file load.

fn lease_write_config(root: &Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

#[test]
fn workspace_lease_settings_are_refused_by_layered_and_staged_loading() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());

    // Valid global-only values load and win.
    lease_write_config(
        global.path(),
        "[lease.intake]\npeer_user = \"global-user\"\n[lease.listener]\nbind = \"127.0.0.1:7789\"\n",
    );
    lease_write_config(workspace.path(), "[decision_channel]\nenabled = true\n");
    let loaded = ResolvedConfig::load(&roots).expect("global lease values load");
    assert_eq!(
        loaded.snapshot.lease_intake_peer_user.as_deref(),
        Some("global-user")
    );
    let effective = load_effective_config(&roots).expect("effective config loads");
    assert!(
        effective
            .values()
            .iter()
            .any(|entry| entry.key == "lease.intake.peer_user"
                && entry.value.as_str() == Some("global-user")),
        "global peer_user is effective"
    );

    for (workspace_body, naming) in [
        (
            "[lease.intake]\npeer_user = \"workspace-user\"\n",
            "lease.intake.peer_user",
        ),
        (
            "[lease.listener]\nbind = \"127.0.0.1:7789\"\n",
            "lease.listener",
        ),
    ] {
        lease_write_config(workspace.path(), workspace_body);
        for refusal in [
            ResolvedConfig::load(&roots)
                .expect_err("ordinary layered load must refuse workspace lease settings")
                .to_string(),
            load_effective_config(&roots)
                .expect_err("effective load must refuse workspace lease settings")
                .to_string(),
        ] {
            assert!(refusal.contains(naming), "{refusal}");
            assert!(refusal.contains("config.toml"), "{refusal}");
            assert!(refusal.contains("not a workspace setting"), "{refusal}");
        }

        // An unrelated staged workspace edit is refused against the same file.
        let workspace_path = workspace.path().join("config.toml");
        let mut store = ConfigStore::open_for_workspace_set(
            &workspace_path,
            &global.path().join("config.toml"),
            WorkspaceInitMode::RequireExisting,
        )
        .expect("open workspace store");
        store
            .set_value("decision_channel.enabled", "true")
            .expect("stage unrelated edit");
        let refusal = store
            .validate_for_set("decision_channel.enabled")
            .expect_err("staged workspace validation must refuse lease settings")
            .to_string();
        assert!(refusal.contains(naming), "{refusal}");

        // An unrelated staged global edit validated against this workspace
        // is refused too, so it cannot leave the workspace unloadable.
        let global_path = global.path().join("config.toml");
        let mut global_store =
            ConfigStore::open(ConfigScope::Global, global_path).expect("open global store");
        global_store
            .set_value("decision_channel.enabled", "true")
            .expect("stage global edit");
        let refusal = global_store
            .validate_global_with_workspace(workspace.path())
            .expect_err("staged global validation must refuse workspace lease settings")
            .to_string();
        assert!(refusal.contains(naming), "{refusal}");
    }

    // A workspace without lease keys loads beside valid global values.
    lease_write_config(workspace.path(), "[decision_channel]\nenabled = true\n");
    ResolvedConfig::load(&roots).expect("clean workspace loads");
}
