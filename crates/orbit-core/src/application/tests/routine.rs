//! Managed-routine provenance across releases [DANI-10392].
//!
//! The upgrade path these cover: a workspace seeded by an earlier release,
//! opted in the documented way (`enabled: true`) and with the retired
//! `hosts:` key dropped as the loader instructs, must converge on
//! `orbit workspace sync` — the retired default retired, a stale default
//! refreshed — while a genuine hand edit is still preserved and reported.

use tempfile::tempdir;

use crate::application::routine::{
    DEFAULT_ROUTINE_FILES, RETIRED_ROUTINE_FILES, SUPERSEDED_ROUTINE_TEMPLATES, ShippedShape,
    seed_default_routines, shipped_shape_of,
};
use crate::application::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, ManagedAssetOutcome,
    ManagedAssetReconciliation, encode_managed_asset_manifest, load_managed_asset_manifest,
    sha256_hex,
};
use orbit_common::protocol::yaml::parse_routine_yaml;

/// Render a shipped template the way a release of that vintage would have
/// written it for `workspace`.
fn render(template: &str, stem: &str, workspace: &str) -> String {
    template.replace(
        "__ORBIT_ROUTINE_NAME__",
        &format!("{}-{workspace}", stem.replace('_', "-")),
    )
}

fn retired_template(stem: &str) -> &'static str {
    RETIRED_ROUTINE_FILES
        .iter()
        .find(|(name, _)| *name == stem)
        .map(|(_, template)| *template)
        .expect("retired template is shipped as a provenance shape")
}

fn superseded_template(stem: &str) -> &'static str {
    SUPERSEDED_ROUTINE_TEMPLATES
        .iter()
        .find(|(name, _)| *name == stem)
        .map(|(_, template)| *template)
        .expect("superseded template is shipped as a provenance shape")
}

fn current_template(stem: &str) -> &'static str {
    DEFAULT_ROUTINE_FILES
        .iter()
        .find(|(name, _)| *name == stem)
        .map(|(_, template)| *template)
        .expect("stem is a currently shipped default")
}

/// Record `body` in the routines manifest as the bytes Orbit wrote for
/// `stem`, exactly as the seeding release would have.
fn record_as_orbit_written(routines_dir: &std::path::Path, stem: &str, body: &str) {
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let mut manifest =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
            .expect("load manifest")
            .expect("seeding records a manifest");
    let digest = sha256_hex(body.as_bytes());
    manifest.assets.insert(stem.to_string(), digest.clone());
    manifest.routine_provenance.remove(stem);
    std::fs::write(
        &manifest_path,
        encode_managed_asset_manifest(&manifest).expect("encode manifest"),
    )
    .expect("write manifest");
}

/// Whether the routines manifest claims `stem` at all.
fn manifest_tracks(routines_dir: &std::path::Path, stem: &str) -> bool {
    load_managed_asset_manifest(
        &routines_dir.join(MANAGED_ASSET_MANIFEST_FILE),
        "routine",
        ManagedAssetLayout::YamlStem,
    )
    .expect("load manifest")
    .is_some_and(|manifest| manifest.assets.contains_key(stem))
}

fn outcome_of(reconciled: &ManagedAssetReconciliation, stem: &str) -> Vec<ManagedAssetOutcome> {
    reconciled
        .actions
        .iter()
        .filter(|action| action.name == stem)
        .map(|action| action.outcome)
        .collect()
}

/// A workspace seeded by the release that last shipped `auto_task_scheduler`,
/// never touched since: sync retires it without the operator moving anything.
#[test]
fn unmodified_previous_release_retired_default_is_retired() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let seeded = render(
        retired_template("auto_task_scheduler"),
        "auto_task_scheduler",
        "workspace",
    );
    let path = routines_dir.join("auto_task_scheduler.yaml");
    std::fs::write(&path, &seeded).expect("write the previous release's routine");
    record_as_orbit_written(&routines_dir, "auto_task_scheduler", &seeded);

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(reconciled.retired, 1);
    assert_eq!(
        outcome_of(&reconciled, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Retired]
    );
    assert!(reconciled.warnings.is_empty(), "{:?}", reconciled.warnings);
    assert!(!path.exists(), "the unmodified retired default is deleted");
    assert!(
        !root
            .path()
            .join(".retired-managed/routines/auto_task_scheduler.yaml")
            .exists(),
        "an unmodified retired default needs no preservation copy"
    );
}

/// The reported case: the operator opted the seeded routine in
/// (`enabled: true`) and dropped the retired `hosts:` key the loader told
/// them to drop. Neither is a local edit, so the file still retires — the
/// upgrade converges instead of demanding a manual move forever.
#[test]
fn lifecycle_edited_retired_default_is_retired_not_called_locally_modified() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    // What the seeding release actually wrote: a host pin, seeded disabled.
    let seeded = render(
        retired_template("auto_task_scheduler"),
        "auto_task_scheduler",
        "workspace",
    )
    .replace(
        "enabled: false\n",
        "enabled: false\nhosts:\n  - daniels-mac-mini.local\n",
    );
    let path = routines_dir.join("auto_task_scheduler.yaml");
    std::fs::write(&path, &seeded).expect("write the previous release's routine");
    record_as_orbit_written(&routines_dir, "auto_task_scheduler", &seeded);

    // The documented lifecycle: opt in, then drop the retired key.
    let lifecycle_edited = seeded
        .replace("enabled: false", "enabled: true")
        .replace("hosts:\n  - daniels-mac-mini.local\n", "");
    assert!(
        parse_routine_yaml(&lifecycle_edited)
            .expect("edited routine parses")
            .enabled
    );
    std::fs::write(&path, &lifecycle_edited).expect("apply the lifecycle edits");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Retired],
        "lifecycle-only differences must not be reported as a local edit"
    );
    assert!(reconciled.warnings.is_empty(), "{:?}", reconciled.warnings);
    assert!(!path.exists());
    // Orbit deletes outright only bytes it can prove it wrote, so the
    // operator's version is kept even though it needed no action from them.
    assert_eq!(
        std::fs::read_to_string(
            root.path()
                .join(".retired-managed/routines/auto_task_scheduler.yaml")
        )
        .expect("a lifecycle variant is copied aside, not destroyed"),
        lifecycle_edited
    );
}

/// A comment an operator added to a shipped default is not a template-owned
/// field, so the routine is still Orbit's — and adoption leaves the file
/// exactly as written, so the comment survives.
#[test]
fn operator_comment_on_a_shipped_default_is_adopted_in_place() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let path = routines_dir.join("dependabot_alert_sweep.yaml");
    let annotated = format!(
        "{}# enabled for the nightly backlog review\n",
        std::fs::read_to_string(&path).expect("read seeded routine")
    );
    std::fs::write(&path, &annotated).expect("annotate the routine");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "dependabot_alert_sweep"),
        vec![ManagedAssetOutcome::Migrated]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread routine"),
        annotated,
        "adoption must not rewrite the operator's comment"
    );
}

/// A default a prior release wrote to disk without recording it in the
/// manifest — the other half of the reported symptom — is adopted rather
/// than reported as a user-authored collision forever.
#[test]
fn untracked_but_orbit_named_default_is_adopted_instead_of_called_a_collision() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let path = routines_dir.join("ci_failure_sweep.yaml");
    let previous = render(
        superseded_template("ci_failure_sweep"),
        "ci_failure_sweep",
        "workspace",
    )
    .replace("enabled: false", "enabled: true");
    std::fs::write(&path, &previous).expect("write the previous release's routine");

    // Drop the manifest entry: the release that wrote the file never recorded
    // it, but the directory as a whole is tracked.
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let mut manifest =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
            .expect("load manifest")
            .expect("seeding records a manifest");
    manifest.assets.remove("ci_failure_sweep");
    manifest.routine_provenance.remove("ci_failure_sweep");
    std::fs::write(
        &manifest_path,
        encode_managed_asset_manifest(&manifest).expect("encode manifest"),
    )
    .expect("write a manifest that never claimed this name");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert!(
        !reconciled
            .warnings
            .iter()
            .any(|warning| warning.contains("collides with bundled default")),
        "Orbit's own file must not be called a user-authored collision: {:?}",
        reconciled.warnings
    );
    assert_eq!(
        outcome_of(&reconciled, "ci_failure_sweep"),
        vec![ManagedAssetOutcome::Refreshed]
    );
    let refreshed = std::fs::read_to_string(&path).expect("read refreshed routine");
    assert!(
        parse_routine_yaml(&refreshed)
            .expect("refreshed routine parses")
            .enabled,
        "the operator's opt-in must survive the refresh"
    );

    // An operator's own routine wearing the bundled filename declares its own
    // name, so it is still reported and preserved.
    let user_authored = previous.replace("ci-failure-sweep-workspace", "my-own-sweep");
    std::fs::write(&path, &user_authored).expect("write a user-authored routine");
    let mut manifest =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
            .expect("load manifest")
            .expect("manifest exists");
    manifest.assets.remove("ci_failure_sweep");
    manifest.routine_provenance.remove("ci_failure_sweep");
    std::fs::write(
        &manifest_path,
        encode_managed_asset_manifest(&manifest).expect("encode manifest"),
    )
    .expect("write manifest");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "ci_failure_sweep"),
        vec![ManagedAssetOutcome::Preserved]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread routine"),
        user_authored
    );
}

/// The distinguishing condition: a field the template owns really was
/// edited, so the file is preserved outside the active catalog and reported.
#[test]
fn hand_edited_retired_default_is_preserved_and_reported() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let seeded = render(
        retired_template("auto_task_scheduler"),
        "auto_task_scheduler",
        "workspace",
    );
    let path = routines_dir.join("auto_task_scheduler.yaml");
    std::fs::write(&path, &seeded).expect("write the previous release's routine");
    record_as_orbit_written(&routines_dir, "auto_task_scheduler", &seeded);

    let hand_edited = seeded.replace(r#"cron: "* * * * *""#, r#"cron: "*/7 * * * *""#);
    assert_ne!(hand_edited, seeded, "fixture must change a template field");
    std::fs::write(&path, &hand_edited).expect("apply a genuine hand edit");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Preserved, ManagedAssetOutcome::Retired]
    );
    assert!(
        reconciled
            .warnings
            .iter()
            .any(|warning| warning.contains("was locally modified")),
        "{:?}",
        reconciled.warnings
    );
    assert!(!path.exists(), "the edited file leaves the active catalog");
    let preserved = root
        .path()
        .join(".retired-managed/routines/auto_task_scheduler.yaml");
    assert_eq!(
        std::fs::read_to_string(&preserved).expect("the hand edit is preserved, not destroyed"),
        hand_edited
    );
}

/// `--check` reports the same classification without writing: no deletion,
/// no preservation move.
#[test]
fn check_mode_reports_retirement_without_touching_the_workspace() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let seeded = render(
        retired_template("auto_task_scheduler"),
        "auto_task_scheduler",
        "workspace",
    )
    .replace("enabled: false", "enabled: true");
    let path = routines_dir.join("auto_task_scheduler.yaml");
    std::fs::write(&path, &seeded).expect("write the previous release's routine");
    record_as_orbit_written(&routines_dir, "auto_task_scheduler", &seeded);

    let identity = crate::application::routine::RoutineSeedIdentity::new("workspace")
        .expect("build seed identity");
    let checked = crate::application::routine::reconcile_default_routines(
        &routines_dir,
        &identity,
        false,
        crate::application::ManagedAssetReconcileMode::Check,
    )
    .expect("check");
    assert_eq!(
        outcome_of(&checked, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Retired]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("check mode leaves the file in place"),
        seeded
    );
}

/// A stale *shipped* default written by an earlier release and since opted
/// in is refreshed onto the current template — and keeps the opt-in. Without
/// that, refreshing would silently disable an operator's enabled routine.
#[test]
fn stale_shipped_default_refreshes_and_keeps_the_operator_opt_in() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let previous = render(superseded_template("task_pilot"), "task_pilot", "workspace");
    let opted_in = previous.replace("enabled: false", "enabled: true");
    let path = routines_dir.join("task_pilot.yaml");
    std::fs::write(&path, &opted_in).expect("write the previous release's routine");
    record_as_orbit_written(&routines_dir, "task_pilot", &previous);

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "task_pilot"),
        vec![ManagedAssetOutcome::Refreshed]
    );
    assert!(reconciled.warnings.is_empty(), "{:?}", reconciled.warnings);

    let refreshed = std::fs::read_to_string(&path).expect("read refreshed routine");
    let definition = parse_routine_yaml(&refreshed).expect("refreshed routine parses");
    assert!(definition.enabled, "the operator's opt-in must survive");
    let current = parse_routine_yaml(&render(
        current_template("task_pilot"),
        "task_pilot",
        "workspace",
    ))
    .expect("current template parses");
    assert_eq!(
        definition.trigger.cron, current.trigger.cron,
        "the refreshed routine takes the current template's cadence"
    );
    assert!(definition.legacy_hosts.is_none());

    // Converged: a second sync has nothing left to do.
    let second = seed_default_routines(&routines_dir, "workspace", false).expect("second sync");
    assert_eq!(second.refreshed, 0);
    assert_eq!(
        outcome_of(&second, "task_pilot"),
        vec![ManagedAssetOutcome::Unchanged]
    );
}

/// A current-shape default whose only difference is the operator's opt-in is
/// adopted in place: no rewrite, no warning, and the next sync is a no-op.
#[test]
fn opted_in_current_default_is_adopted_without_rewriting_it() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let path = routines_dir.join("dependabot_alert_sweep.yaml");
    let seeded = std::fs::read_to_string(&path).expect("read seeded routine");
    let opted_in = seeded.replace("enabled: false", "enabled: true");
    std::fs::write(&path, &opted_in).expect("apply the dashboard toggle's edit");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "dependabot_alert_sweep"),
        vec![ManagedAssetOutcome::Migrated]
    );
    assert!(reconciled.warnings.is_empty(), "{:?}", reconciled.warnings);
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread routine"),
        opted_in,
        "adoption must not rewrite the operator's opt-in"
    );

    let second = seed_default_routines(&routines_dir, "workspace", false).expect("second sync");
    assert_eq!(
        outcome_of(&second, "dependabot_alert_sweep"),
        vec![ManagedAssetOutcome::Unchanged]
    );
}

/// An edit to a field the template owns is still a local edit, even on a
/// currently shipped default.
#[test]
fn hand_edited_shipped_default_is_still_preserved() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let path = routines_dir.join("dependabot_alert_sweep.yaml");
    let seeded = std::fs::read_to_string(&path).expect("read seeded routine");
    let edited = seeded.replace(r#"cron: "25 3 * * *""#, r#"cron: "*/5 * * * *""#);
    assert_ne!(edited, seeded, "fixture must change a template field");
    std::fs::write(&path, &edited).expect("apply a genuine hand edit");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "dependabot_alert_sweep"),
        vec![ManagedAssetOutcome::Preserved]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread routine"),
        edited,
        "a hand-edited definition is never rewritten"
    );
}

/// Shape classification itself: every shipped, superseded, and retired
/// template is recognised, and an operator's own routine is not.
#[test]
fn shipped_shapes_are_classified_by_template_owned_fields() {
    for (stem, template) in DEFAULT_ROUTINE_FILES {
        let body = render(template, stem, "workspace");
        assert_eq!(
            shipped_shape_of(stem, &body),
            Some(ShippedShape::Current),
            "{stem} must be recognised as its current shipped shape"
        );
        assert_eq!(
            shipped_shape_of(stem, &body.replace("enabled: false", "enabled: true")),
            Some(ShippedShape::Current),
            "{stem} opted in is still the current shape"
        );
    }
    for (stem, template) in SUPERSEDED_ROUTINE_TEMPLATES {
        assert_eq!(
            shipped_shape_of(stem, &render(template, stem, "workspace")),
            Some(ShippedShape::Superseded)
        );
    }
    for (stem, template) in RETIRED_ROUTINE_FILES {
        assert_eq!(
            shipped_shape_of(stem, &render(template, stem, "workspace")),
            Some(ShippedShape::Retired)
        );
    }

    // A template's own fields changed: not a shipped shape.
    let edited = render(
        current_template("dependabot_alert_sweep"),
        "dependabot_alert_sweep",
        "workspace",
    )
    .replace(r#"cron: "25 3 * * *""#, r#"cron: "*/5 * * * *""#);
    assert_eq!(shipped_shape_of("dependabot_alert_sweep", &edited), None);
    // Nor is a file that does not parse as a routine.
    assert_eq!(
        shipped_shape_of("dependabot_alert_sweep", "not: a routine\n"),
        None
    );
}

/// The deliberate exception: an overwriting seed (`--force`) restores the
/// shipped template verbatim, opt-in included. Ordinary convergence keeps the
/// operator's `enabled` setting; a destructive re-init does not.
#[test]
fn overwriting_seed_restores_the_template_default_enabled() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let path = routines_dir.join("dependabot_alert_sweep.yaml");
    let opted_in = std::fs::read_to_string(&path)
        .expect("read seeded routine")
        .replace("enabled: false", "enabled: true");
    std::fs::write(&path, &opted_in).expect("opt in");
    // Adopt the opt-in first, so the overwriting seed is reconciling against
    // recorded provenance rather than an untracked file.
    seed_default_routines(&routines_dir, "workspace", false).expect("adopt the opt-in");

    seed_default_routines(&routines_dir, "workspace", true).expect("overwriting seed");
    let restored = std::fs::read_to_string(&path).expect("read restored routine");
    assert!(
        !parse_routine_yaml(&restored)
            .expect("restored routine parses")
            .enabled,
        "`--force` restores the shipped template, opt-in included"
    );
    assert_eq!(
        restored,
        render(
            current_template("dependabot_alert_sweep"),
            "dependabot_alert_sweep",
            "workspace"
        )
    );
}

/// The gap this closes [DANI-10502]: the same retired default, on disk with
/// no manifest entry at all — a release that wrote the file without recording
/// the write, or a manifest since reset. The tracked loop cannot see it and it
/// wears no shipped name, so before this it stayed in the active catalog
/// forever while every surface advised a sync that reported `unchanged`.
#[test]
fn untracked_retired_default_is_retired_with_a_preserved_copy() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let seeded = render(
        retired_template("auto_task_scheduler"),
        "auto_task_scheduler",
        "workspace",
    );
    let path = routines_dir.join("auto_task_scheduler.yaml");
    std::fs::write(&path, &seeded).expect("write the previous release's routine");
    assert!(
        !manifest_tracks(&routines_dir, "auto_task_scheduler"),
        "the fixture is the untracked case"
    );

    // `--check` classifies it without touching the workspace.
    let identity = crate::application::routine::RoutineSeedIdentity::new("workspace")
        .expect("build seed identity");
    let checked = crate::application::routine::reconcile_default_routines(
        &routines_dir,
        &identity,
        false,
        crate::application::ManagedAssetReconcileMode::Check,
    )
    .expect("check");
    assert_eq!(
        outcome_of(&checked, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Retired]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("check mode leaves the file in place"),
        seeded
    );

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(reconciled.retired, 1);
    assert_eq!(
        outcome_of(&reconciled, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Retired]
    );
    assert!(reconciled.warnings.is_empty(), "{:?}", reconciled.warnings);
    assert!(
        !path.exists(),
        "the retired default leaves the active catalog"
    );
    // Nothing recorded these bytes as Orbit's, so they are moved aside rather
    // than deleted outright.
    assert_eq!(
        std::fs::read_to_string(
            root.path()
                .join(".retired-managed/routines/auto_task_scheduler.yaml")
        )
        .expect("an untracked retired default is copied aside, not destroyed"),
        seeded
    );

    // Converged: the next sync has nothing left to retire.
    let second = seed_default_routines(&routines_dir, "workspace", false).expect("second sync");
    assert_eq!(second.retired, 0);
    assert!(outcome_of(&second, "auto_task_scheduler").is_empty());
}

/// The distinguishing condition for an untracked file: an operator's own
/// routine wearing a retired default's filename is never Orbit's to retire, so
/// it is preserved in place and reported with the step that actually clears
/// it — not with a sync that would leave it exactly where it is.
#[test]
fn user_authored_routine_at_a_retired_stem_is_preserved_with_actionable_advice() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let user_authored = render(
        retired_template("auto_task_scheduler"),
        "auto_task_scheduler",
        "workspace",
    )
    .replace("auto-task-scheduler-workspace", "my-own-scheduler")
    .replace(r#"cron: "* * * * *""#, r#"cron: "*/7 * * * *""#);
    let path = routines_dir.join("auto_task_scheduler.yaml");
    std::fs::write(&path, &user_authored).expect("write the operator's own routine");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(reconciled.retired, 0);
    assert_eq!(
        outcome_of(&reconciled, "auto_task_scheduler"),
        vec![ManagedAssetOutcome::Preserved]
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread routine"),
        user_authored,
        "a routine Orbit did not write is never moved or rewritten"
    );
    let warning = reconciled
        .warnings
        .iter()
        .find(|warning| warning.contains("auto_task_scheduler.yaml"))
        .unwrap_or_else(|| panic!("the operator must be told: {:?}", reconciled.warnings));
    assert!(
        warning.contains("delete the file or retarget it"),
        "the warning names a step that changes something: {warning}"
    );
}
