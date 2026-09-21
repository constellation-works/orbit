//! Grants decide the surface: a plugin whose manifest asks for something the
//! operator has not granted registers inactive with a diagnostic, and
//! `orbit plugin enable --grant` is what activates it (design §4.1, §4.3).

use orbit_types::plugin::{PluginGrant, PluginStatus};
use orbit_types::telemetry::AuditEventStatus;

use super::super::{
    PluginAddOptions, PluginEnableOptions, disable_plugin, enable_plugin, install_plugin,
    plugin_doctor, show_plugin,
};
use super::fixture::{PluginFixture, PluginSpecFixture};

fn install(fixture: &PluginFixture, spec: PluginSpecFixture<'_>) {
    let source = fixture.write_plugin(spec);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
}

fn permission(summary: &super::super::PluginSummary, grant: PluginGrant) -> (Option<String>, bool) {
    let row = summary
        .permissions
        .iter()
        .find(|permission| permission.grant == grant)
        .unwrap_or_else(|| panic!("a row for {grant}"));
    (row.requested.clone(), row.granted)
}

#[test]
fn an_ungranted_request_registers_the_tool_inactive_until_it_is_granted() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").requesting_fs_write(),
    );

    let runtime = fixture.reopen();
    let refusal = fixture
        .call(&runtime, "demo.hello")
        .expect_err("an inactive tool is not callable");
    assert!(
        refusal.to_string().contains("--grant fs"),
        "the refusal names the missing grant: {refusal}"
    );
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Inactive);
    let diagnostic = summary.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("`fs`")
            && diagnostic.contains("spec.permissions.fs")
            && diagnostic.contains("--grant fs"),
        "{diagnostic}"
    );
    // `orbit plugin show` lists fs as requested and not granted.
    assert_eq!(
        permission(&summary, PluginGrant::Fs),
        (Some("write={{plugin_state}}".to_string()), false)
    );
    assert_eq!(permission(&summary, PluginGrant::Network), (None, false));
    assert!(!summary.tools.is_empty() && summary.tools.iter().all(|tool| !tool.active));

    enable_plugin(&runtime, "demo", &grant_options(&["fs"])).expect("grant fs");

    let runtime = fixture.reopen();
    fixture
        .call(&runtime, "demo.hello")
        .expect("granting fs activates the tool");
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active);
    assert_eq!(summary.diagnostic, None);
    assert_eq!(
        permission(&summary, PluginGrant::Fs),
        (Some("write={{plugin_state}}".to_string()), true)
    );
    assert!(summary.tools.iter().all(|tool| tool.active));
}

#[test]
fn an_unknown_grant_name_is_refused_rather_than_recorded() {
    let fixture = PluginFixture::new();
    install(&fixture, PluginSpecFixture::new("demo", "demo"));
    let runtime = fixture.reopen();
    let error = enable_plugin(&runtime, "demo", &grant_options(&["wifi"]))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("wifi") && error.contains("unsandboxed"),
        "{error}"
    );
    assert!(
        show_plugin(&runtime, "demo")
            .expect("show")
            .granted
            .is_empty()
    );
}

#[test]
fn sandbox_none_refuses_without_the_grant_and_is_a_doctor_finding_with_it() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("loose", "loose").unsandboxed(),
    );

    let runtime = fixture.reopen();
    let refusal = fixture
        .call(&runtime, "loose.hello")
        .expect_err("`sandbox: none` without the grant refuses");
    assert!(refusal.to_string().contains("`unsandboxed`"), "{refusal}");
    let summary = show_plugin(&runtime, "loose").expect("show");
    assert_eq!(summary.status, PluginStatus::Inactive);
    assert!(
        !summary.unsandboxed,
        "not granted, so not running unconfined"
    );
    let diagnostic = summary.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("`unsandboxed`") && diagnostic.contains("backend.sandbox: none"),
        "{diagnostic}"
    );
    // Before the grant, doctor reports the missing step rather than the risk.
    let finding = plugin_doctor(&runtime)
        .expect("doctor")
        .into_iter()
        .find(|result| result.plugin == "loose")
        .expect("a doctor row");
    assert!(finding.message.contains("`unsandboxed`"), "{finding:?}");

    enable_plugin(&runtime, "loose", &grant_options(&["unsandboxed"])).expect("grant");

    let runtime = fixture.reopen();
    fixture
        .call(&runtime, "loose.hello")
        .expect("granting unsandboxed activates the tool");
    let summary = show_plugin(&runtime, "loose").expect("show");
    assert_eq!(summary.status, PluginStatus::Active);
    assert!(summary.unsandboxed);
    let finding = plugin_doctor(&runtime)
        .expect("doctor")
        .into_iter()
        .find(|result| result.plugin == "loose")
        .expect("a doctor row");
    assert_eq!(finding.status, PluginStatus::Active);
    assert!(
        finding.message.contains("runs unsandboxed") && finding.message.contains("loose"),
        "an active unsandboxed plugin is still a finding: {finding:?}"
    );

    // A disabled plugin is not a sandbox finding: it runs nothing.
    disable_plugin(&runtime, "loose").expect("disable");
    let runtime = fixture.reopen();
    let finding = plugin_doctor(&runtime)
        .expect("doctor")
        .into_iter()
        .find(|result| result.plugin == "loose")
        .expect("a doctor row");
    assert!(!finding.message.contains("runs unsandboxed"), "{finding:?}");
}

/// `--grant a,b` as the lifecycle takes it.
fn grant_options(grants: &[&str]) -> PluginEnableOptions {
    PluginEnableOptions {
        grants: grants.iter().map(|grant| (*grant).to_string()).collect(),
        force: false,
    }
}

/// A plugin that can write `orbit.db` can write its own `plugins` row. The
/// grants it puts there are not authority: the loader refuses the row, the
/// operator sees a `doctor` finding, and the refusal is in the audit trail
/// [ORB-12778].
///
/// The row write below is the store's own, not `orbit plugin enable` — the same
/// effect as the `UPDATE plugins SET grants_json='["unsandboxed"]'` in the
/// finding, expressed through the seam a backend reaches with a writable store.
#[test]
fn grants_injected_into_the_store_row_never_become_an_unconfined_plugin() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("loose", "loose").unsandboxed(),
    );

    // Enabled, nothing granted: refused for the missing grant, as today.
    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "loose").expect("show").status,
        PluginStatus::Inactive
    );

    runtime
        .stores()
        .plugins()
        .set_plugin_enabled("loose", true, &["unsandboxed".to_string()])
        .expect("the row write itself succeeds");

    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "loose").expect("show");
    assert_eq!(summary.status, PluginStatus::Inactive);
    assert!(
        !summary.unsandboxed && summary.granted.is_empty(),
        "the injected grant is not reported as authority: {summary:?}"
    );
    assert!(
        summary.permissions.iter().all(|row| !row.granted),
        "{:?}",
        summary.permissions
    );
    let diagnostic = summary.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("`unsandboxed`") && diagnostic.contains("orbit plugin enable loose"),
        "{diagnostic}"
    );
    fixture
        .call(&runtime, "loose.hello")
        .expect_err("a refused row puts no tool on the surface");

    // Visible to an operator, not a silent skip.
    let finding = plugin_doctor(&runtime)
        .expect("doctor")
        .into_iter()
        .find(|result| result.plugin == "loose")
        .expect("a doctor row");
    assert_eq!(finding.status, PluginStatus::Inactive);
    assert_eq!(finding.message, diagnostic);

    // And recorded durably.
    let denials = runtime
        .list_audit_events_with_kind(
            None,
            None,
            Some("plugin".to_string()),
            Some(AuditEventStatus::Denied),
            None,
            10,
        )
        .expect("audit events");
    let denial = denials
        .iter()
        .find(|event| event.target_id.as_deref() == Some("loose"))
        .expect("the refusal was audited");
    assert_eq!(denial.command, "plugin.load");
    assert!(
        denial
            .arguments_json
            .as_deref()
            .is_some_and(|arguments| arguments.contains("unsandboxed")),
        "the row records the set that was claimed: {:?}",
        denial.arguments_json
    );
    let provenance = denial.plugin.as_ref().expect("the row names the plugin");
    assert_eq!(provenance.name, "loose");
    assert!(
        provenance.grants.is_empty(),
        "the refused plugin ran under nothing: {:?}",
        provenance.grants
    );

    // Re-authorizing the same set through the one command that may is what
    // makes it effective again.
    enable_plugin(&runtime, "loose", &grant_options(&["unsandboxed"])).expect("authorize");
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "loose").expect("show");
    assert_eq!(summary.status, PluginStatus::Active);
    assert!(summary.unsandboxed);
    fixture
        .call(&runtime, "loose.hello")
        .expect("an authorized grant activates the tool");
}

/// Enable, disable, re-enable and a reinstall all keep the row and its
/// authorization record in step, so the check never refuses a plugin an
/// operator maintained through the ordinary commands.
#[test]
fn the_ordinary_lifecycle_keeps_the_row_authorized() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo").requesting_fs_write());
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "demo").expect("show").status,
        PluginStatus::Active,
        "`add --enable --grant` authorizes what it records"
    );

    disable_plugin(&runtime, "demo").expect("disable");
    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "demo").expect("show").status,
        PluginStatus::Disabled
    );

    enable_plugin(&runtime, "demo", &grant_options(&["fs"])).expect("re-enable");
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active);
    assert_eq!(summary.granted, ["fs"]);

    // A reinstall over the top carries the row's grants forward; it is not an
    // authorization, and it must not invalidate the one already recorded.
    install_plugin(
        &runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            force: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("reinstall");
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active, "{summary:?}");
    assert_eq!(summary.granted, ["fs"]);
}
