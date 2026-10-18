// Unknown settings must remain loadable, visible and quiet on repeated loads.
#[test]
fn unknown_config_keys_warn_once_per_file_and_suggest_known_keys() {
    if !isolated("unknown_config_keys_warn_once_per_file_and_suggest_known_keys") {
        return;
    }
    let directory = tempfile::tempdir().expect("config roots");
    let roots = ConfigRoots::new(
        directory.path().join("global"),
        directory.path().join("workspace"),
    );
    let body =
        "[workflow]\nbase_branc = 'main'\n[task.approval]\ndelegate_approval = true\n[future]\n";
    for root in [roots.global(), roots.workspace()] {
        fs::create_dir_all(root).expect("config directory");
        fs::write(root.join("config.toml"), body).expect("config file");
    }
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(buffer.clone()))
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..3 {
            ResolvedConfig::load(&roots).expect("unknown keys do not prevent loading");
        }
    });
    let logs = String::from_utf8(buffer.lock().expect("logs").clone()).expect("utf8");
    let warnings: Vec<_> = logs
        .lines()
        .filter(|line| line.contains("unknown config key"))
        .collect();
    assert_eq!(
        warnings.len(),
        6,
        "one warning per file and unknown path: {logs}"
    );
    for root in [roots.global(), roots.workspace()] {
        let path = redact_home_dir(&root.join("config.toml").display().to_string());
        for key in ["workflow.base_branc", "task.approval", "future"] {
            assert_eq!(
                warnings
                    .iter()
                    .filter(|line| line.contains(&path) && line.contains(key))
                    .count(),
                1,
                "{logs}"
            );
        }
    }
    assert!(
        warnings
            .iter()
            .filter(|line| line.contains("workflow.base_branc"))
            .all(|line| line.contains("workflow.base_branch")),
        "typo warnings name the nearest known key: {logs}"
    );
}

#[test]
fn unknown_config_diagnosis_preserves_known_and_extension_settings() {
    if !isolated("unknown_config_diagnosis_preserves_known_and_extension_settings") {
        return;
    }
    let directory = tempfile::tempdir().expect("config root");
    let path = directory.path().join("config.toml");
    let body = r#"
[workflow]
default_crew = "fixture"
[workflow.tag_instructions]
"any.tag" = { implement = "notes.md" }
[crews.fixture]
provider = "codex"
model = "fixture"
effrot = "high"
[plugins.future]
anything = { nested = true }
[runtime]
[review]
before_pr = true
reviewers = 2
[operation]
future = true
review_minutes = 7
[semantic]
anything = true
"#;
    fs::write(&path, body).expect("config");
    ResolvedConfig::load(&ConfigRoots::new(directory.path(), directory.path()))
        .expect("unknown review fields load");
    let keys = orbit_config::unknown_config_keys_at(&path).expect("diagnosis");
    assert_eq!(
        keys.iter().map(|key| key.key.as_str()).collect::<Vec<_>>(),
        [
            "crews.fixture.effrot",
            "operation.future",
            "review.reviewers"
        ]
    );
    assert_eq!(keys[0].suggestion.as_deref(), Some("crews.fixture.effort"));
    let mut store = ConfigStore::open(ConfigScope::Global, &path).expect("store");
    for key in &keys {
        assert!(store.remove_unknown_key(key).expect("remove unknown"));
        assert!(
            !store
                .remove_unknown_key(key)
                .expect("stale diagnosis removes nothing")
        );
    }
    store.validate().expect("known fields remain valid");
    store.save().expect("save");
    let updated: toml::Value =
        toml::from_str(&fs::read_to_string(&path).expect("updated")).expect("TOML");
    assert_eq!(
        updated["plugins"]["future"]["anything"]["nested"].as_bool(),
        Some(true)
    );
    assert_eq!(
        updated["workflow"]["tag_instructions"]["any.tag"]["implement"].as_str(),
        Some("notes.md")
    );
    assert_eq!(updated["review"]["before_pr"].as_bool(), Some(true));
    assert!(orbit_config::unknown_config_keys_present(&updated).is_empty());
}

#[test]
fn unknown_literal_dotted_key_cleanup_never_removes_a_known_path() {
    if !isolated("unknown_literal_dotted_key_cleanup_never_removes_a_known_path") {
        return;
    }
    let directory = tempfile::tempdir().expect("config root");
    let path = directory.path().join("config.toml");
    fs::write(
        &path,
        "\"workflow.base_branch\" = 'stale'\n[workflow]\nbase_branch = 'agent-main'\n",
    )
    .expect("config");
    let keys = orbit_config::unknown_config_keys_at(&path).expect("diagnosis");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, "\"workflow.base_branch\"");
    let mut store = ConfigStore::open(ConfigScope::Global, &path).expect("store");
    assert!(
        store
            .remove_unknown_key(&keys[0])
            .expect("remove literal key")
    );
    store.save().expect("save");
    let config =
        ResolvedConfig::load(&ConfigRoots::new(directory.path(), directory.path())).expect("load");
    assert_eq!(config.snapshot.workflow_base_branch, "agent-main");
}
