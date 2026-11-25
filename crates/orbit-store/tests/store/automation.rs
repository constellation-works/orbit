//  Automation checkpoints through public composition, a separate store area
//  from task admission and readiness. Mutable fixtures run in isolated children.

fn automation_isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_AUTOMATION_STORE_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return false;
    }
    let home = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output =
        process::run_bounded_capped(&mut command, test_env::CHILD_TEST_DEADLINE, 64 * 1024)
            .expect("run isolated automation fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    true
}

/// Upgrade admission reports each frozen feature schema at the version a store
/// opened by this binary holds, so binaries from before the store ledger
/// absorbed the feature ledgers keep comparing them. The automation repair at
/// v5 stays data-only [ORB-15260]: a binary whose registry stops at v4 keeps
/// running beside this one.
#[test]
fn upgrade_admission_reports_the_feature_versions_a_store_holds() {
    if automation_isolated("upgrade_admission_reports_the_feature_versions_a_store_holds") {
        return;
    }
    let base = Store::open_in_memory().unwrap();
    let held: std::collections::BTreeMap<String, u32> = base
        .connection()
        .lock()
        .unwrap()
        .prepare("SELECT feature, MAX(version) FROM feature_schema_meta GROUP BY feature")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let compiled = compose::compiled_compatibility();
    assert_eq!(compiled.features, held);
    let floor = &compiled.feature_floors["automation"];
    assert!(
        floor.version < 5,
        "release_misread_pilot_failures must stay data-only, or older binaries refuse: {floor:?}"
    );
}
