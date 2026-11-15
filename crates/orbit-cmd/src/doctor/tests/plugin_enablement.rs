use crate::doctor::WorkspaceDoctorStatus;
use crate::doctor::report::plugin_enablement_row_for;

#[test]
fn misplaced_global_plugin_enablement_is_an_error_with_the_exact_fix() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("config.toml"),
        "[machine]\nname = \"owner-host\"\n\n[plugin_enablement]\ngraph = false\n",
    )
    .expect("write config");

    let row = plugin_enablement_row_for(root.path());
    assert_eq!(row.check_name, "plugin-enablement");
    assert_eq!(row.status, WorkspaceDoctorStatus::Error, "{row:?}");
    assert!(row.message.contains("owner-host"), "{row:?}");
    assert!(row.message.contains("config.toml"), "{row:?}");
    let fix = row.remediation.expect("remediation");
    assert!(fix.contains("Remove [plugin_enablement] from"), "{fix}");
    assert!(fix.contains("--scope workspace"), "{fix}");
}

#[test]
fn absent_or_clean_host_config_is_ok() {
    let root = tempfile::tempdir().expect("tempdir");
    let row = plugin_enablement_row_for(root.path());
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok, "{row:?}");

    std::fs::write(root.path().join("config.toml"), "[machine]\nname = \"h\"\n")
        .expect("write config");
    let row = plugin_enablement_row_for(root.path());
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok, "{row:?}");
}
