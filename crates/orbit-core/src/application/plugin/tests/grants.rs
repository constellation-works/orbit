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
use crate::runtime::plugin_grants::plugin_grant_witness_path;

fn install(fixture: &PluginFixture, spec: PluginSpecFixture<'_>) {
    let source = fixture.write_plugin(spec);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
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
fn add_enable_and_show_share_the_missing_grant_projection() {
    let fixture = PluginFixture::new();
    let source =
        fixture.write_plugin(PluginSpecFixture::new("guarded", "guarded").requesting_fs_write());
    let added = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("add enabled plugin");
    assert_eq!(added.status, PluginStatus::Inactive, "{added:?}");
    let added_diagnostic = added.diagnostic.clone().expect("add diagnostic");

    let enabled = enable_plugin(&fixture.runtime, "guarded", &PluginEnableOptions::default())
        .expect("re-enable without the missing grant")
        .summary;
    assert_eq!(enabled.status, PluginStatus::Inactive, "{enabled:?}");
    assert_eq!(
        enabled.diagnostic.as_deref(),
        Some(added_diagnostic.as_str())
    );

    let shown = show_plugin(&fixture.reopen(), "guarded").expect("show");
    assert_eq!(shown.status, PluginStatus::Inactive, "{shown:?}");
    assert_eq!(shown.diagnostic.as_deref(), Some(added_diagnostic.as_str()));
}

#[test]
fn enable_warns_for_each_grant_the_manifest_does_not_request() {
    let fixture = PluginFixture::new();
    install(&fixture, PluginSpecFixture::new("demo", "demo"));

    let result = enable_plugin(&fixture.runtime, "demo", &grant_options(&["fs", "network"]))
        .expect("valid but unrequested grants remain explicit operator consent");
    assert_eq!(result.summary.status, PluginStatus::Active);
    assert_eq!(result.warnings.len(), 2, "{:?}", result.warnings);
    for grant in ["fs", "network"] {
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains(grant) && warning.contains("does not request")),
            "the warning must name unrequested grant {grant}: {:?}",
            result.warnings
        );
    }
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

/// `--grant none` is the operator's way to revoke every grant explicitly,
/// unlike omitting `--grant`, which preserves whatever is recorded. The
/// witness has to be rewritten for the empty set too, or the row would be
/// refused as an unauthorized change the next time it loads.
#[test]
fn grant_none_records_an_explicit_empty_set_and_rewrites_the_witness() {
    let fixture = PluginFixture::new();
    install(&fixture, PluginSpecFixture::new("demo", "demo"));

    enable_plugin(&fixture.runtime, "demo", &grant_options(&["fs"])).expect("grant fs first");
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    assert_eq!(stored.grants, ["fs"]);

    enable_plugin(&fixture.runtime, "demo", &grant_options(&["none"]))
        .expect("revoke to the explicit empty set");
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    assert!(stored.grants.is_empty(), "{:?}", stored.grants);

    // Reopening re-verifies the row against its witness; if the witness had
    // not been rewritten for the empty set, this would refuse the plugin as
    // an unauthorized change rather than show it active with nothing granted.
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active, "{summary:?}");
    assert!(summary.granted.is_empty(), "{:?}", summary.granted);
}

/// `--grant requested` is shorthand for typing out exactly what the manifest
/// asks for, without an operator having to read `orbit plugin show` first and
/// retype each name.
#[test]
fn grant_requested_grants_exactly_the_manifests_request() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo")
            .requesting_fs_write()
            .unsandboxed(),
    );

    enable_plugin(&fixture.runtime, "demo", &grant_options(&["requested"]))
        .expect("grant exactly what the manifest requests");
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    let mut grants = stored.grants.clone();
    grants.sort();
    assert_eq!(grants, ["fs", "unsandboxed"], "{:?}", stored.grants);

    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active, "{summary:?}");
    assert!(summary.unsandboxed);
}

/// A path-scoped `fs` grant records its roots, and the roots are inside the
/// authorization witness: re-scoping a plugin is a change to the authorized
/// set, not a detail beside it, so it needs fresh consent the same way adding
/// a grant does [ORB-12840].
#[test]
fn a_path_scoped_fs_grant_records_its_roots_and_moves_the_witness() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").requesting_fs_write(),
    );

    // The manifest requests `{{plugin_state}}`; the operator allows one
    // directory inside it.
    enable_plugin(
        &fixture.runtime,
        "demo",
        &grant_options(&["fs={{plugin_state}}/cache"]),
    )
    .expect("grant fs scoped to one root");
    let stored = |fixture: &PluginFixture| {
        fixture
            .runtime
            .stores()
            .plugins()
            .get_plugin("demo")
            .expect("read plugin row")
            .expect("installed plugin")
    };
    assert_eq!(stored(&fixture).grants, ["fs={{plugin_state}}/cache"]);

    let witness = |fixture: &PluginFixture| {
        std::fs::read_to_string(plugin_grant_witness_path(
            &fixture.runtime.global_root(),
            "demo",
        ))
        .expect("witness")
    };
    let scoped_witness = witness(&fixture);

    // The plugin still loads: a scoped `fs` is `fs` recorded, so the
    // manifest's request is satisfied and the row verifies against its
    // witness on a fresh open.
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active, "{summary:?}");
    assert_eq!(summary.granted, ["fs={{plugin_state}}/cache"]);

    // `show` puts the manifest's request beside the operator's roots, which
    // is the delta an operator needs in order to see what was narrowed.
    let fs = summary
        .permissions
        .iter()
        .find(|permission| permission.grant == PluginGrant::Fs)
        .expect("an fs row");
    assert_eq!(fs.requested.as_deref(), Some("write={{plugin_state}}"));
    assert!(fs.granted);
    assert_eq!(
        fs.granted_roots.as_deref(),
        Some(["{{plugin_state}}/cache".to_string()].as_slice())
    );

    // Widening the roots is a different authorized set, so the witness moves.
    enable_plugin(
        &fixture.runtime,
        "demo",
        &grant_options(&["fs={{plugin_state}}"]),
    )
    .expect("re-scope to the whole request");
    assert_eq!(stored(&fixture).grants, ["fs={{plugin_state}}"]);
    assert_ne!(
        witness(&fixture),
        scoped_witness,
        "the witness has to cover the roots, or a backend could widen its own \
         scope in the row and still verify"
    );

    // And the unscoped shorthand is a third distinct set, not a synonym for
    // having granted every root the manifest happens to request today.
    enable_plugin(&fixture.runtime, "demo", &grant_options(&["fs"])).expect("shorthand");
    assert_eq!(stored(&fixture).grants, ["fs"]);
    assert_ne!(witness(&fixture), scoped_witness);
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(summary.status, PluginStatus::Active, "{summary:?}");
    let fs = summary
        .permissions
        .iter()
        .find(|permission| permission.grant == PluginGrant::Fs)
        .expect("an fs row");
    assert!(fs.granted);
    assert_eq!(
        fs.granted_roots, None,
        "`--grant fs` records no roots: it is the manifest's request at this digest"
    );
}

/// Narrowing a grant is the operator's decision, so `doctor` stays quiet
/// about it — but a requested root that overlaps *nothing* granted will never
/// open, and the plugin would fail somewhere inside itself rather than at
/// load. That one is a finding.
#[test]
fn doctor_names_a_requested_root_the_grant_leaves_out_entirely() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("demo", "demo").requesting_two_fs_writes(),
    );

    enable_plugin(
        &fixture.runtime,
        "demo",
        &grant_options(&["fs={{plugin_state}}/kept"]),
    )
    .expect("grant one of the two requested roots");

    let runtime = fixture.reopen();
    let findings = plugin_doctor(&runtime).expect("doctor");
    let scoped_out: Vec<&str> = findings
        .iter()
        .filter(|row| row.message.contains("outside every root this host granted"))
        .map(|row| row.message.as_str())
        .collect();
    assert_eq!(scoped_out.len(), 1, "{findings:?}");
    assert!(
        scoped_out[0].contains("{{plugin_state}}/dropped")
            && scoped_out[0].contains("spec.permissions.fs.write[1]"),
        "{}",
        scoped_out[0]
    );
    assert!(
        !scoped_out[0].contains("{{plugin_state}}/kept"),
        "the granted root is not a finding: {}",
        scoped_out[0]
    );

    // The unscoped shorthand grants the whole request, so nothing is dropped.
    enable_plugin(&fixture.runtime, "demo", &grant_options(&["fs"])).expect("shorthand");
    let runtime = fixture.reopen();
    let findings = plugin_doctor(&runtime).expect("doctor");
    assert!(
        !findings
            .iter()
            .any(|row| row.message.contains("outside every root this host granted")),
        "{findings:?}"
    );
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

/// The recovery command named by the refusal must state a new grant set, not
/// sign whatever a writer injected into the store row. Otherwise the witness
/// turns the diagnostic's own remediation into an authorization bypass.
#[test]
fn enable_with_grants_replaces_an_unauthorized_stored_superset() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        PluginSpecFixture::new("loose", "loose").unsandboxed(),
    );

    let injected = ["fs".to_string(), "unsandboxed".to_string()];
    fixture
        .runtime
        .stores()
        .plugins()
        .set_plugin_enabled("loose", true, &injected)
        .expect("inject a grant the operator never authorized");

    let runtime = fixture.reopen();
    let refusal = show_plugin(&runtime, "loose")
        .expect("show")
        .diagnostic
        .expect("the injected row is refused");
    assert!(
        refusal.contains("orbit plugin enable loose --grant <grants>"),
        "the recovery command exercised below must be the one the refusal names: {refusal}"
    );

    enable_plugin(&runtime, "loose", &grant_options(&["fs"]))
        .expect("authorize the intended narrower set");
    let stored = runtime
        .stores()
        .plugins()
        .get_plugin("loose")
        .expect("read plugin row")
        .expect("installed plugin");
    assert_eq!(stored.grants, ["fs"]);

    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "loose").expect("show narrowed set");
    assert_eq!(summary.granted, ["fs"]);
    assert!(
        !summary.unsandboxed,
        "the recovery command must not bless the injected grant: {summary:?}"
    );

    // Restoring the injected row must fail closed again. This proves the new
    // witness covers only `fs`, not the superset that preceded the command.
    runtime
        .stores()
        .plugins()
        .set_plugin_enabled("loose", true, &injected)
        .expect("restore the injected superset");
    let runtime = fixture.reopen();
    let summary = show_plugin(&runtime, "loose").expect("show refused superset");
    assert!(
        summary.granted.is_empty() && !summary.unsandboxed,
        "the narrowed witness must not cover the injected superset: {summary:?}"
    );
    assert!(
        summary
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("do not match")),
        "the restored superset must mismatch the narrowed witness: {summary:?}"
    );
}

#[test]
fn add_and_enable_with_grants_both_replace_the_recorded_set() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect("initial add with an explicit grant set");

    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            force: true,
            enable: true,
            grants: vec!["network".to_string()],
        },
    )
    .expect("replacement add with a new explicit grant set");
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    assert_eq!(
        stored.grants,
        ["network"],
        "add --enable --grant replaces the old set"
    );

    enable_plugin(&fixture.runtime, "demo", &grant_options(&["fs"]))
        .expect("enable with a replacement grant set");
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    assert_eq!(
        stored.grants,
        ["fs"],
        "an explicit enable grant list replaces, just like add --enable --grant"
    );
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
            digest: None,
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

    enable_plugin(&runtime, "demo", &PluginEnableOptions::default())
        .expect("re-enable without changing grants");
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
            digest: None,
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
