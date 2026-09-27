//! Managed-routine provenance across releases [DANI-10392].
//!
//! The upgrade path these cover: a workspace seeded by an earlier release,
//! opted in the documented way (`enabled: true`) and with the retired
//! `hosts:` key dropped as the loader instructs, must converge on
//! `orbit workspace sync` — the retired default retired, a stale default
//! refreshed — while a genuine hand edit is still preserved and reported.

use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::automation::members::StateTriggerKind;
use orbit_types::workflow::{OverlapPolicy, RoutineTarget};
use tempfile::tempdir;

use super::super::materialize::{reconcile_default_routines, seed_default_routines};
use super::super::parse_cron;
use super::super::seed::{
    BASE_BRANCH_PLACEHOLDER, DEFAULT_ROUTINE_FILES, OWNER_MACHINE_PLACEHOLDER,
    RETIRED_ROUTINE_FILES, ROUTINE_NAME_PLACEHOLDER, RoutineSeedIdentity,
    SUPERSEDED_ROUTINE_TEMPLATES,
};
use super::super::template::{ShippedShape, shipped_shape_of};
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, ManagedAssetOutcome,
    ManagedAssetReconciliation, encode_managed_asset_manifest, load_managed_asset_manifest,
};

/// Render a shipped template the way a release of that vintage would have
/// written it for `workspace` on the test host (`hm_test`, observing `main`,
/// the identity `seed_default_routines` uses).
pub(super) fn render(template: &str, stem: &str, workspace: &str) -> String {
    template
        .replace(
            "__ORBIT_ROUTINE_NAME__",
            &format!("{}-{workspace}", stem.replace('_', "-")),
        )
        .replace(OWNER_MACHINE_PLACEHOLDER, "hm_test")
        .replace(BASE_BRANCH_PLACEHOLDER, "main")
}

/// The cron-form `task_pilot` template the release before [ORB-12745]
/// shipped: the shape an existing workspace holds before it upgrades.
fn cron_task_pilot_template() -> &'static str {
    SUPERSEDED_ROUTINE_TEMPLATES
        .iter()
        .filter(|(name, _)| *name == "task_pilot")
        .map(|(_, template)| *template)
        .find(|template| template.contains("cron: \"*/40 * * * *\""))
        .expect("the cron task_pilot form is shipped as a superseded shape")
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

pub(super) fn current_template(stem: &str) -> &'static str {
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

/// Record `rendered` (from `template`) as the provenance the seeding release
/// wrote for `stem`, with the name-only binding that release recorded.
fn record_provenance(routines_dir: &std::path::Path, stem: &str, template: &str, rendered: &str) {
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let mut manifest =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
            .expect("load manifest")
            .expect("seeding records a manifest");
    let name = parse_routine_yaml(rendered).expect("rendered parses").name;
    manifest
        .assets
        .insert(stem.to_string(), sha256_hex(rendered.as_bytes()));
    manifest.routine_provenance.insert(
        stem.to_string(),
        crate::application::managed_assets::RoutineAssetProvenance {
            template_digest: sha256_hex(template.as_bytes()),
            rendered_digest: sha256_hex(rendered.as_bytes()),
            binding: crate::application::managed_assets::RoutineMaterializationBinding {
                name,
                owner_machine: None,
                branch: None,
            },
        },
    );
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

    let identity =
        RoutineSeedIdentity::new("workspace", "hm_test", "main").expect("build seed identity");
    let checked = reconcile_default_routines(
        &routines_dir,
        &identity,
        false,
        crate::application::managed_assets::ManagedAssetReconcileMode::Check,
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
        definition.trigger, current.trigger,
        "the refreshed routine takes the current template's trigger"
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
    let identity =
        RoutineSeedIdentity::new("workspace", "hm_test", "main").expect("build seed identity");
    let checked = reconcile_default_routines(
        &routines_dir,
        &identity,
        false,
        crate::application::managed_assets::ManagedAssetReconcileMode::Check,
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

#[test]
fn seeded_routines_are_valid_disabled_and_workspace_unique() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join(".orbit/routines");
    let seeded =
        seed_default_routines(&routines_dir, "My Repo!", true).expect("seed default routines");
    assert_eq!(seeded.refreshed, DEFAULT_ROUTINE_FILES.len());

    for (stem, target) in [
        ("ci_failure_sweep", "ci_failure_sweep_pipeline"),
        ("dependabot_alert_sweep", "dependabot_alert_sweep_pipeline"),
        ("task_pilot", "task_pilot_pipeline"),
        ("ship_sweep", "workspace_ship_pipeline"),
        ("worktree_gc", "worktree_gc_pipeline"),
    ] {
        let yaml = std::fs::read_to_string(routines_dir.join(format!("{stem}.yaml")))
            .expect("read seeded routine");
        let definition = parse_routine_yaml(&yaml).expect("seeded routine parses fail-closed");
        assert_eq!(
            definition.name,
            format!("{}-my-repo", stem.replace('_', "-"))
        );
        assert_eq!(definition.target, RoutineTarget::Job(target.to_string()));
        assert_eq!(definition.policy.overlap, OverlapPolicy::Forbid);
        assert!(!definition.enabled);
    }

    // Terminal failed-run triage is retired: no default seeds its job.
    assert!(!routines_dir.join("task_triage.yaml").exists());

    // Task-pilot is state-triggered [ORB-12745]: it names this host as its
    // owner and observes the registered base branch, and its 90-minute
    // timeout covers a one-task partition plus deterministic preparation/apply.
    let pilot = std::fs::read_to_string(routines_dir.join("task_pilot.yaml"))
        .expect("read task-pilot routine");
    assert!(
        !pilot.contains("__ORBIT_"),
        "every placeholder must resolve at seed time:\n{pilot}"
    );
    let pilot = parse_routine_yaml(&pilot).expect("task-pilot routine parses");
    assert!(pilot.trigger.cron.is_empty());
    let trigger = pilot
        .trigger
        .state
        .as_ref()
        .expect("task-pilot seeds a state trigger");
    assert_eq!(trigger.kind, StateTriggerKind::PreparationEligible);
    assert_eq!(trigger.owner_machine, "hm_test");
    assert_eq!(trigger.branch, "main");
    assert_eq!(
        (
            trigger.debounce_minutes,
            trigger.max_wait_minutes,
            trigger.max_items,
            trigger.retries,
            trigger.deadline_minutes
        ),
        (2, 10, 50, 1, 90)
    );
    assert!(
        trigger.eligibility.is_default(),
        "the seeded eligibility block spells out the default predicate"
    );
    assert_eq!(pilot.policy.timeout_minutes, 90);
    assert_eq!(pilot.policy.overlap, OverlapPolicy::Forbid);
    assert_eq!(pilot.policy.retries.max, 1);
    assert_eq!(pilot.policy.retries.backoff_minutes, 5);

    let ship =
        std::fs::read_to_string(routines_dir.join("ship_sweep.yaml")).expect("read ship routine");
    let ship = parse_routine_yaml(&ship).expect("ship routine parses");
    assert_eq!(
        ship.trigger.missed_run,
        orbit_types::workflow::MissedRunPolicy::Skip
    );
    assert_eq!(ship.trigger.cron, "*/20 * * * *");
    parse_cron(&ship.trigger.cron).expect("ship cron parses");

    let gc = std::fs::read_to_string(routines_dir.join("worktree_gc.yaml"))
        .expect("read worktree GC routine");
    let gc = parse_routine_yaml(&gc).expect("worktree GC routine parses");
    assert!(!gc.enabled);
    assert_eq!(gc.policy.overlap, OverlapPolicy::Forbid);
    assert_eq!(gc.trigger.cron, "35 * * * *");

    // The CI-failure sweep is hourly and must not stack with any other
    // shipped default: two schedules on the same minute would have the
    // seeded routines contend for the same host on every fire.
    let sweep = std::fs::read_to_string(routines_dir.join("ci_failure_sweep.yaml"))
        .expect("read CI-failure sweep routine");
    let sweep = parse_routine_yaml(&sweep).expect("CI-failure sweep routine parses");
    assert!(!sweep.enabled);
    assert_eq!(sweep.trigger.cron, "5 * * * *");
    assert_ne!(sweep.trigger.cron, gc.trigger.cron);
    assert_eq!(
        sweep.trigger.missed_run,
        orbit_types::workflow::MissedRunPolicy::Skip
    );
    assert_eq!(sweep.policy.overlap, OverlapPolicy::Forbid);
    parse_cron(&sweep.trigger.cron).expect("CI-failure sweep cron parses");

    let dependabot = std::fs::read_to_string(routines_dir.join("dependabot_alert_sweep.yaml"))
        .expect("read Dependabot sweep routine");
    let dependabot = parse_routine_yaml(&dependabot).expect("Dependabot sweep routine parses");
    assert!(!dependabot.enabled);
    assert_eq!(dependabot.trigger.cron, "25 3 * * *");
    assert_eq!(dependabot.policy.overlap, OverlapPolicy::Forbid);
    for occupied in ["5 * * * *", "15 * * * *", "35 * * * *", "*/20 * * * *"] {
        assert_ne!(dependabot.trigger.cron, occupied);
    }
    parse_cron(&dependabot.trigger.cron).expect("Dependabot sweep cron parses");
}

#[test]
fn seeding_preserves_existing_files_unless_overwrite() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("first seed");
    let path = routines_dir.join("worktree_gc.yaml");
    std::fs::write(&path, "user edited").expect("simulate user edit");

    let seeded = seed_default_routines(&routines_dir, "workspace", false).expect("re-seed");
    assert_eq!(seeded.refreshed, 0);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "user edited",
        "plain re-init must not clobber user edits"
    );

    seed_default_routines(&routines_dir, "workspace", true).expect("refresh defaults");
    let refreshed = std::fs::read_to_string(&path).expect("read refreshed");
    let definition = parse_routine_yaml(&refreshed).expect("refreshed routine parses");
    assert_eq!(definition.name, "worktree-gc-workspace");
    assert_eq!(
        definition.target,
        RoutineTarget::Job("worktree_gc_pipeline".to_string())
    );
    assert!(!definition.enabled);
}

#[test]
fn plain_reinit_adds_a_new_missing_default_without_rewriting_existing_files() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("first seed");

    let existing = routines_dir.join("ci_failure_sweep.yaml");
    let original = std::fs::read(&existing).expect("read existing routine bytes");
    let missing = routines_dir.join("task_pilot.yaml");
    std::fs::remove_file(&missing).expect("remove newly introduced routine");

    let seeded = seed_default_routines(&routines_dir, "workspace", false).expect("plain re-init");
    assert_eq!(seeded.refreshed, 1, "only the missing default is created");
    assert_eq!(
        std::fs::read(&existing).expect("read existing routine bytes"),
        original,
        "plain re-init must preserve existing routines byte-for-byte"
    );

    let pilot = parse_routine_yaml(
        &std::fs::read_to_string(&missing).expect("read newly seeded task-pilot routine"),
    )
    .expect("newly seeded task-pilot routine parses");
    assert!(!pilot.enabled);
}

/// The recorded digest covers the *rendered* document, so re-seeding
/// unchanged embedded content for the same workspace must not rewrite a
/// single file — even under `overwrite`. A steady-state bootstrap can then
/// run against a read-only routines directory.
#[test]
fn reseeding_unchanged_rendered_content_is_a_no_op_not_a_rewrite() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", true).expect("first seed");

    let before: Vec<(std::path::PathBuf, std::time::SystemTime)> = DEFAULT_ROUTINE_FILES
        .iter()
        .map(|(stem, _)| {
            let path = routines_dir.join(format!("{stem}.yaml"));
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .expect("read seeded routine mtime");
            (path, modified)
        })
        .collect();

    let reseeded = seed_default_routines(&routines_dir, "workspace", true)
        .expect("re-seed unchanged rendered content");
    assert_eq!(reseeded.refreshed, 0, "unchanged routines must not rewrite");
    assert_eq!(reseeded.retired, 0);
    assert!(reseeded.warnings.is_empty());

    for (path, modified) in before {
        let current = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .expect("read routine mtime after re-seed");
        assert_eq!(
            current,
            modified,
            "re-seed rewrote `{}` despite identical rendered content",
            path.display()
        );
    }

    // Seeding is machine-independent [ORB-12236], so re-seeding the same
    // workspace name on another host stays a no-op.
    let elsewhere = seed_default_routines(&routines_dir, "workspace", true)
        .expect("re-seed as another host would");
    assert_eq!(elsewhere.refreshed, 0);
}

#[test]
fn fresh_routine_seeding_matches_rendered_canonical_templates() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed canonical routines");

    for (stem, template) in DEFAULT_ROUTINE_FILES {
        let rendered = render(template, stem, "workspace");
        let seeded = std::fs::read_to_string(routines_dir.join(format!("{stem}.yaml")))
            .expect("read seeded routine");
        assert_eq!(
            seeded, rendered,
            "freshly seeded {stem} must match its rendered template"
        );
        assert!(
            !seeded.contains(ROUTINE_NAME_PLACEHOLDER)
                && !seeded.contains(OWNER_MACHINE_PLACEHOLDER)
                && !seeded.contains(BASE_BRANCH_PLACEHOLDER),
            "{stem} left a placeholder unresolved"
        );
    }
}

/// Seed-time resolution of the state trigger's owner and branch [ORB-12745]:
/// the identity's machine id and base branch land in `task_pilot.yaml` and
/// nowhere else, so the cron defaults stay host-independent [ORB-12236].
#[test]
fn state_routine_seeds_this_hosts_owner_and_the_registered_base_branch() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    let identity =
        RoutineSeedIdentity::new("workspace", "hm_seed_host", "agent-main").expect("seed identity");
    reconcile_default_routines(
        &routines_dir,
        &identity,
        false,
        crate::application::managed_assets::ManagedAssetReconcileMode::Apply,
    )
    .expect("seed default routines");

    let pilot = parse_routine_yaml(
        &std::fs::read_to_string(routines_dir.join("task_pilot.yaml")).expect("read task-pilot"),
    )
    .expect("task-pilot parses");
    let trigger = pilot.trigger.state.expect("state trigger");
    assert_eq!(trigger.owner_machine, "hm_seed_host");
    assert_eq!(trigger.branch, "agent-main");

    for (stem, _) in DEFAULT_ROUTINE_FILES
        .iter()
        .filter(|(stem, _)| *stem != "task_pilot")
    {
        let seeded = std::fs::read_to_string(routines_dir.join(format!("{stem}.yaml")))
            .expect("read seeded routine");
        assert!(
            !seeded.contains("hm_seed_host") && !seeded.contains("agent-main"),
            "{stem} must not carry host state"
        );
    }

    let manifest = load_managed_asset_manifest(
        &routines_dir.join(MANAGED_ASSET_MANIFEST_FILE),
        "routine",
        ManagedAssetLayout::YamlStem,
    )
    .expect("load manifest")
    .expect("seeding records a manifest");
    let pilot_binding = &manifest.routine_provenance["task_pilot"].binding;
    assert_eq!(pilot_binding.owner_machine.as_deref(), Some("hm_seed_host"));
    assert_eq!(pilot_binding.branch.as_deref(), Some("agent-main"));
    let gc_binding = &manifest.routine_provenance["worktree_gc"].binding;
    assert_eq!(gc_binding.owner_machine, None);
    assert_eq!(gc_binding.branch, None);

    // A blank machine id or an unobservable branch cannot render a valid
    // state trigger, so the identity refuses them up front.
    assert!(RoutineSeedIdentity::new("workspace", " ", "main").is_err());
    assert!(RoutineSeedIdentity::new("workspace", "hm_seed_host", "").is_err());
    assert!(RoutineSeedIdentity::new("workspace", "hm_seed_host", "no branch").is_err());
}

/// The upgrade `orbit workspace sync` performs for a workspace seeded with
/// the cron task-pilot form [ORB-12745]: an unmodified file, and one whose
/// only edit is the opt-in, refresh onto the state form owned by this host,
/// keeping `enabled`; a second sync is a no-op.
#[test]
fn unmodified_cron_task_pilot_upgrades_to_the_state_form_on_sync() {
    for opted_in in [false, true] {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

        let previous = render(cron_task_pilot_template(), "task_pilot", "workspace");
        let on_disk = if opted_in {
            previous.replace("enabled: false", "enabled: true")
        } else {
            previous.clone()
        };
        let path = routines_dir.join("task_pilot.yaml");
        std::fs::write(&path, &on_disk).expect("write the previous release's routine");
        record_as_orbit_written(&routines_dir, "task_pilot", &previous);

        let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
        assert!(
            outcome_of(&reconciled, "task_pilot").contains(&ManagedAssetOutcome::Refreshed),
            "opted_in={opted_in}: {:?}",
            reconciled.actions
        );
        assert!(reconciled.warnings.is_empty(), "{:?}", reconciled.warnings);

        let refreshed = std::fs::read_to_string(&path).expect("read refreshed routine");
        assert!(!refreshed.contains("__ORBIT_"), "{refreshed}");
        let definition = parse_routine_yaml(&refreshed).expect("refreshed routine parses");
        assert_eq!(
            definition.enabled, opted_in,
            "the operator's opt-in must survive"
        );
        assert!(definition.trigger.cron.is_empty());
        let trigger = definition
            .trigger
            .state
            .expect("upgraded to the state form");
        assert_eq!(trigger.kind, StateTriggerKind::PreparationEligible);
        assert_eq!(trigger.owner_machine, "hm_test");
        assert_eq!(trigger.branch, "main");

        let second = seed_default_routines(&routines_dir, "workspace", false).expect("second sync");
        assert_eq!(second.refreshed, 0);
        assert_eq!(
            outcome_of(&second, "task_pilot"),
            vec![ManagedAssetOutcome::Unchanged]
        );
    }
}

/// The same upgrade for a workspace whose manifest already carries routine
/// provenance — the name-only binding the cron release recorded is completed
/// with this host's owner and branch rather than reported as drift.
#[test]
fn tracked_cron_task_pilot_upgrades_and_completes_its_recorded_binding() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let template = cron_task_pilot_template();
    let previous = render(template, "task_pilot", "workspace");
    let path = routines_dir.join("task_pilot.yaml");
    std::fs::write(&path, &previous).expect("write the previous release's routine");
    record_provenance(&routines_dir, "task_pilot", template, &previous);

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "task_pilot"),
        vec![ManagedAssetOutcome::Refreshed],
        "{:?}",
        reconciled.actions
    );
    let definition =
        parse_routine_yaml(&std::fs::read_to_string(&path).expect("read refreshed routine"))
            .expect("refreshed routine parses");
    let trigger = definition
        .trigger
        .state
        .expect("upgraded to the state form");
    assert_eq!(trigger.owner_machine, "hm_test");
    assert_eq!(trigger.branch, "main");

    let manifest = load_managed_asset_manifest(
        &routines_dir.join(MANAGED_ASSET_MANIFEST_FILE),
        "routine",
        ManagedAssetLayout::YamlStem,
    )
    .expect("load manifest")
    .expect("manifest present");
    let binding = &manifest.routine_provenance["task_pilot"].binding;
    assert_eq!(binding.owner_machine.as_deref(), Some("hm_test"));
    assert_eq!(binding.branch.as_deref(), Some("main"));

    let second = seed_default_routines(&routines_dir, "workspace", false).expect("second sync");
    assert_eq!(
        outcome_of(&second, "task_pilot"),
        vec![ManagedAssetOutcome::Unchanged]
    );
}

/// A cron task-pilot whose template-owned fields were edited is the
/// operator's: sync leaves it alone and says so.
#[test]
fn hand_edited_cron_task_pilot_is_preserved_and_reported_on_sync() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

    let previous = render(cron_task_pilot_template(), "task_pilot", "workspace");
    let edited = previous.replace("*/40 * * * *", "*/15 * * * *");
    assert_ne!(edited, previous, "fixture must change a template field");
    let path = routines_dir.join("task_pilot.yaml");
    std::fs::write(&path, &edited).expect("write the operator's cron routine");
    record_as_orbit_written(&routines_dir, "task_pilot", &previous);

    let reconciled = seed_default_routines(&routines_dir, "workspace", false).expect("sync");
    assert_eq!(
        outcome_of(&reconciled, "task_pilot"),
        vec![ManagedAssetOutcome::Preserved]
    );
    let report = reconciled
        .actions
        .iter()
        .find(|action| action.name == "task_pilot")
        .and_then(|action| action.detail.clone())
        .expect("a preserved routine is reported");
    assert!(report.contains("preserved"), "{report}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread routine"),
        edited,
        "a hand-edited definition is never rewritten"
    );
}

/// A state-form definition an operator authored by hand — the ws_orbit
/// migration of 2026-09-21 — that differs from the template only in
/// comments, description or `enabled` is adopted; one with a different
/// predicate or budget is a local edit.
#[test]
fn hand_authored_state_task_pilot_is_adopted_or_preserved_by_shape() {
    let current = render(current_template("task_pilot"), "task_pilot", "workspace");
    assert_eq!(
        shipped_shape_of(
            "task_pilot",
            &current.replace("enabled: false", "enabled: true")
        ),
        Some(ShippedShape::Current)
    );
    // Another host's owner is still the shipped shape: the document's own
    // binding is what the template is rendered against.
    assert_eq!(
        shipped_shape_of("task_pilot", &current.replace("hm_test", "hm_elsewhere")),
        Some(ShippedShape::Current)
    );
    assert_eq!(
        shipped_shape_of(
            "task_pilot",
            &current.replace("debounce_minutes: 2", "debounce_minutes: 5")
        ),
        None
    );
    assert_eq!(
        shipped_shape_of(
            "task_pilot",
            &current.replace("require_tags: []", "require_tags: [pilot-me]")
        ),
        None
    );
    // The cron form is a superseded shape, never the current one.
    assert_eq!(
        shipped_shape_of(
            "task_pilot",
            &render(cron_task_pilot_template(), "task_pilot", "workspace")
        ),
        Some(ShippedShape::Superseded)
    );
}

#[test]
fn task_pilot_reseeding_preserves_workspace_overrides() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed canonical routines");
    let path = routines_dir.join("task_pilot.yaml");
    let edited = std::fs::read_to_string(&path)
        .expect("read task-pilot routine")
        .replace("enabled: false", "enabled: true")
        .replace("debounce_minutes: 2", "debounce_minutes: 5");
    let definition = parse_routine_yaml(&edited).expect("customized routine parses");
    assert!(definition.enabled);
    assert_eq!(
        definition
            .trigger
            .state
            .expect("state trigger")
            .debounce_minutes,
        5
    );
    std::fs::write(&path, &edited).expect("write operator overrides");

    seed_default_routines(&routines_dir, "workspace", false)
        .expect("reseed without overwriting workspace choices");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read preserved routine"),
        edited
    );
}

/// A routines directory seeded before routines carried managed-asset
/// provenance has no manifest at all. Customizing a seeded routine —
/// `enabled: true` — is the documented lifecycle, so it must be adopted
/// into provenance rather than accused of colliding with the bundled
/// default it came from [ORB-11154].
#[test]
fn manifestless_customized_routines_are_adopted_rather_than_called_collisions() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false)
        .expect("seed a pre-provenance workspace");
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    std::fs::remove_file(&manifest_path).expect("drop the manifest to predate provenance");

    let customized = routines_dir.join("ci_failure_sweep.yaml");
    let edited = std::fs::read_to_string(&customized)
        .expect("read seeded routine")
        .replace("enabled: false", "enabled: true");
    assert!(
        edited.contains("enabled: true"),
        "fixture must opt the routine in"
    );
    std::fs::write(&customized, &edited).expect("simulate the documented customization");

    let adopted = seed_default_routines(&routines_dir, "workspace", false)
        .expect("reconcile the pre-provenance directory");
    assert!(
        adopted.warnings.is_empty(),
        "customizing a seeded routine must not warn: {:?}",
        adopted.warnings
    );
    assert!(
        !adopted
            .actions
            .iter()
            .any(|action| action.outcome == ManagedAssetOutcome::Preserved),
        "no routine may be reported as a user-authored collision: {:?}",
        adopted.actions
    );
    assert!(adopted.actions.iter().any(|action| {
        action.name == "ci_failure_sweep" && action.outcome == ManagedAssetOutcome::Migrated
    }));
    assert_eq!(
        std::fs::read_to_string(&customized).expect("reread routine"),
        edited,
        "adoption must not rewrite the operator's routine"
    );

    let manifest =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
            .expect("load adopted manifest")
            .expect("adoption records a manifest");
    let provenance = manifest
        .routine_provenance
        .get("ci_failure_sweep")
        .expect("the customized routine gains provenance");
    assert_eq!(provenance.rendered_digest, sha256_hex(edited.as_bytes()));
    assert_eq!(provenance.binding.name, "ci-failure-sweep-workspace");

    // Provenance now owns the file, so convergence is a no-op instead of
    // repeating the same complaint on every run.
    let second =
        seed_default_routines(&routines_dir, "workspace", false).expect("second reconcile");
    assert!(second.warnings.is_empty());
    assert_eq!(second.refreshed, 0);
    assert!(second.actions.iter().any(|action| {
        action.name == "ci_failure_sweep" && action.outcome == ManagedAssetOutcome::Unchanged
    }));
}

/// The distinguishing condition: once Orbit tracks the directory, a shipped
/// name that the manifest does not claim really is user-authored, and is
/// still reported [ORB-11154].
#[test]
fn user_authored_collision_is_still_reported_when_the_manifest_tracks_the_directory() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    seed_default_routines(&routines_dir, "workspace", false).expect("seed default routines");
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

    let user_authored = routines_dir.join("ci_failure_sweep.yaml");
    let content = std::fs::read_to_string(&user_authored)
        .expect("read routine")
        .replace("ci-failure-sweep-workspace", "my-own-sweep");
    std::fs::write(&user_authored, &content).expect("write a user-authored routine");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false)
        .expect("reconcile a tracked directory");
    assert!(
        reconciled
            .warnings
            .iter()
            .any(|warning| warning.contains("collides with bundled default")),
        "a tracked directory must still report a user-authored collision: {:?}",
        reconciled.warnings
    );
    assert!(reconciled.actions.iter().any(|action| {
        action.name == "ci_failure_sweep" && action.outcome == ManagedAssetOutcome::Preserved
    }));
    assert_eq!(
        std::fs::read_to_string(&user_authored).expect("reread routine"),
        content
    );
}

/// Adoption needs a binding Orbit can re-render later. A file that does not
/// parse as a routine yields none, so it stays a reported collision even in
/// a manifest-less directory [ORB-11154].
#[test]
fn manifestless_unparseable_collision_is_still_reported() {
    let root = tempdir().expect("create tempdir");
    let routines_dir = root.path().join("routines");
    std::fs::create_dir_all(&routines_dir).expect("create routines dir");
    let path = routines_dir.join("ci_failure_sweep.yaml");
    std::fs::write(
        &path,
        "not: a routine
",
    )
    .expect("write an unmanageable file");

    let reconciled = seed_default_routines(&routines_dir, "workspace", false)
        .expect("reconcile a manifest-less directory");
    assert!(
        reconciled
            .warnings
            .iter()
            .any(|warning| warning.contains("collides with bundled default")),
        "{:?}",
        reconciled.warnings
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("reread file"),
        "not: a routine\n"
    );
}

/// Routine catalog confinement: a link anywhere on a routine's route —
/// the definition, the catalog, its manifest, or the preservation directory —
/// is refused and reported, and nothing outside the catalog is created,
/// overwritten, moved, or deleted.
#[cfg(unix)]
mod confinement {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use tempfile::tempdir;

    use super::super::super::materialize::{
        reconcile_default_routines, seed_default_routines, write_confined_routine,
    };
    use super::super::super::seed::{DEFAULT_ROUTINE_FILES, RoutineSeedIdentity};
    use super::{
        cron_task_pilot_template, current_template, manifest_tracks, outcome_of,
        record_as_orbit_written, render, retired_template, superseded_template,
    };
    use crate::application::managed_assets::{
        MANAGED_ASSET_MANIFEST_FILE, ManagedAssetOutcome, ManagedAssetReconcileMode,
        ManagedAssetReconciliation,
    };

    fn sync(routines_dir: &Path, mode: ManagedAssetReconcileMode) -> ManagedAssetReconciliation {
        let identity =
            RoutineSeedIdentity::new("workspace", "hm_test", "main").expect("build seed identity");
        reconcile_default_routines(routines_dir, &identity, false, mode).expect("sync")
    }

    fn assert_is_link(path: &Path) {
        assert!(
            std::fs::symlink_metadata(path)
                .expect("inspect the link")
                .file_type()
                .is_symlink(),
            "'{}' must be left as the operator's link",
            path.display()
        );
    }

    fn only_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("list outside dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The reported path: a tracked default replaced by a dangling link whose
    /// target's parent exists. It must not read as "missing" and be recreated
    /// at the external target, in either mode.
    #[test]
    fn dangling_definition_link_is_refused_not_recreated_outside() {
        let root = tempdir().expect("create tempdir");
        let outside = tempdir().expect("create outside dir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

        let path = routines_dir.join("worktree_gc.yaml");
        std::fs::remove_file(&path).expect("drop the seeded definition");
        let target = outside.path().join("new.yaml");
        symlink(&target, &path).expect("plant a dangling link");

        for mode in [
            ManagedAssetReconcileMode::Check,
            ManagedAssetReconcileMode::Apply,
        ] {
            let reconciled = sync(&routines_dir, mode);
            assert_eq!(
                outcome_of(&reconciled, "worktree_gc"),
                vec![ManagedAssetOutcome::Preserved]
            );
            assert_eq!(reconciled.warnings.len(), 1, "{:?}", reconciled.warnings);
            assert_eq!(reconciled.refreshed, 0);
            assert!(
                !target.exists(),
                "no routine is written outside the catalog"
            );
            assert_is_link(&path);
        }
        assert!(
            manifest_tracks(&routines_dir, "worktree_gc"),
            "the recorded provenance survives the refusal"
        );

        // A fresh catalog with no manifest at all refuses the same way.
        let fresh = root.path().join("fresh");
        std::fs::create_dir(&fresh).expect("create fresh catalog");
        symlink(&target, fresh.join("worktree_gc.yaml")).expect("plant a dangling link");
        let seeded = seed_default_routines(&fresh, "workspace", false).expect("seed");
        assert_eq!(
            outcome_of(&seeded, "worktree_gc"),
            vec![ManagedAssetOutcome::Preserved]
        );
        assert!(
            !target.exists(),
            "no routine is written outside the catalog"
        );
        assert!(
            fresh.join("task_pilot.yaml").is_file(),
            "ordinary defaults beside the refused link still seed"
        );
    }

    /// An existing link to external bytes that match the recorded provenance
    /// of a stale template would otherwise be refreshed in place, overwriting
    /// the external file.
    #[test]
    fn existing_definition_link_is_never_refreshed_through() {
        let root = tempdir().expect("create tempdir");
        let outside = tempdir().expect("create outside dir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

        let previous = render(superseded_template("task_pilot"), "task_pilot", "workspace");
        let target = outside.path().join("task_pilot.yaml");
        std::fs::write(&target, &previous).expect("write external bytes");
        let path = routines_dir.join("task_pilot.yaml");
        std::fs::remove_file(&path).expect("drop the seeded definition");
        symlink(&target, &path).expect("link the definition outside");
        record_as_orbit_written(&routines_dir, "task_pilot", &previous);

        let reconciled = sync(&routines_dir, ManagedAssetReconcileMode::Apply);
        assert_eq!(
            outcome_of(&reconciled, "task_pilot"),
            vec![ManagedAssetOutcome::Preserved]
        );
        assert_eq!(reconciled.refreshed, 0);
        assert_eq!(
            std::fs::read_to_string(&target).expect("read external bytes"),
            previous,
            "the external file is not overwritten"
        );
        assert_is_link(&path);

        // The same holds for a lifecycle variant in a manifest-less catalog,
        // which would otherwise be refreshed onto the current template.
        let fresh = root.path().join("fresh");
        std::fs::create_dir(&fresh).expect("create fresh catalog");
        let cron = render(cron_task_pilot_template(), "task_pilot", "workspace")
            .replace("enabled: false", "enabled: true");
        let cron_target = outside.path().join("cron.yaml");
        std::fs::write(&cron_target, &cron).expect("write external bytes");
        symlink(&cron_target, fresh.join("task_pilot.yaml")).expect("link outside");
        let seeded = seed_default_routines(&fresh, "workspace", false).expect("seed");
        assert_eq!(
            outcome_of(&seeded, "task_pilot"),
            vec![ManagedAssetOutcome::Preserved]
        );
        assert_eq!(
            std::fs::read_to_string(&cron_target).expect("read external bytes"),
            cron
        );
    }

    /// The lifecycle refresh and every other routine write share one writer,
    /// which refuses a link at the definition or catalog even when a caller
    /// skipped inspection.
    #[test]
    fn confined_writer_refuses_links() {
        let root = tempdir().expect("create tempdir");
        let outside = tempdir().expect("create outside dir");
        let routines_dir = root.path().join("routines");
        std::fs::create_dir(&routines_dir).expect("create catalog");
        let body = render(current_template("task_pilot"), "task_pilot", "workspace");

        let dangling = routines_dir.join("dangling.yaml");
        symlink(outside.path().join("dangling.yaml"), &dangling).expect("plant link");
        let existing_target = outside.path().join("existing.yaml");
        std::fs::write(&existing_target, "external\n").expect("write external bytes");
        let existing = routines_dir.join("existing.yaml");
        symlink(&existing_target, &existing).expect("plant link");
        write_confined_routine(&dangling, &body).expect_err("dangling link is refused");
        write_confined_routine(&existing, &body).expect_err("existing link is refused");

        let linked_catalog = root.path().join("linked");
        symlink(outside.path(), &linked_catalog).expect("link the catalog");
        write_confined_routine(&linked_catalog.join("task_pilot.yaml"), &body)
            .expect_err("a linked catalog is refused");

        assert_eq!(only_entries(outside.path()), vec!["existing.yaml"]);
        assert_eq!(
            std::fs::read_to_string(&existing_target).expect("read external bytes"),
            "external\n"
        );

        // An ordinary regular file and a missing catalog are still written.
        let fresh = root.path().join("fresh").join("task_pilot.yaml");
        write_confined_routine(&fresh, &body).expect("create a missing catalog and file");
        write_confined_routine(&fresh, &body).expect("overwrite a regular file");
        assert_eq!(std::fs::read_to_string(&fresh).expect("read"), body);
    }

    /// A linked catalog or manifest leaves every definition and its
    /// provenance behind the link, so the whole catalog is refused.
    #[test]
    fn linked_catalog_or_manifest_is_refused_whole() {
        let root = tempdir().expect("create tempdir");
        let outside = tempdir().expect("create outside dir");
        let routines_dir = root.path().join("routines");
        symlink(outside.path(), &routines_dir).expect("link the catalog outside");

        let reconciled = sync(&routines_dir, ManagedAssetReconcileMode::Apply);
        assert_eq!(reconciled.refreshed, 0);
        for (name, _) in DEFAULT_ROUTINE_FILES {
            assert_eq!(
                outcome_of(&reconciled, name),
                vec![ManagedAssetOutcome::Preserved]
            );
        }
        assert!(
            only_entries(outside.path()).is_empty(),
            "neither a routine nor a manifest is written through the link"
        );

        let catalog = root.path().join("catalog");
        seed_default_routines(&catalog, "workspace", false).expect("seed current defaults");
        let manifest = catalog.join(MANAGED_ASSET_MANIFEST_FILE);
        let external_manifest = outside.path().join("manifest.json");
        std::fs::rename(&manifest, &external_manifest).expect("move the manifest out");
        symlink(&external_manifest, &manifest).expect("link the manifest");
        let before = std::fs::read(&external_manifest).expect("read manifest");
        let worktree_gc = catalog.join("worktree_gc.yaml");
        std::fs::remove_file(&worktree_gc).expect("drop a definition");

        let reconciled = sync(&catalog, ManagedAssetReconcileMode::Apply);
        assert_eq!(
            outcome_of(&reconciled, "worktree_gc"),
            vec![ManagedAssetOutcome::Preserved]
        );
        assert!(!worktree_gc.exists(), "nothing is judged behind the link");
        assert_eq!(std::fs::read(&external_manifest).expect("read"), before);
        assert_is_link(&manifest);
    }

    /// Retirement never deletes or moves through a link, and never moves an
    /// operator's copy into a linked preservation directory.
    #[test]
    fn retirement_is_confined_to_the_catalog_and_its_preservation_route() {
        let root = tempdir().expect("create tempdir");
        let outside = tempdir().expect("create outside dir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

        // A tracked retired default linked outside, byte-exact to the record.
        let seeded = render(
            retired_template("auto_task_scheduler"),
            "auto_task_scheduler",
            "workspace",
        );
        let target = outside.path().join("auto_task_scheduler.yaml");
        std::fs::write(&target, &seeded).expect("write external bytes");
        let path = routines_dir.join("auto_task_scheduler.yaml");
        symlink(&target, &path).expect("link the retired default outside");
        record_as_orbit_written(&routines_dir, "auto_task_scheduler", &seeded);

        for mode in [
            ManagedAssetReconcileMode::Check,
            ManagedAssetReconcileMode::Apply,
        ] {
            let reconciled = sync(&routines_dir, mode);
            assert_eq!(reconciled.retired, 0);
            assert_eq!(
                outcome_of(&reconciled, "auto_task_scheduler"),
                vec![ManagedAssetOutcome::Preserved]
            );
        }
        assert_is_link(&path);
        assert_eq!(std::fs::read_to_string(&target).expect("read"), seeded);
        assert!(
            manifest_tracks(&routines_dir, "auto_task_scheduler"),
            "retirement completes once the link is replaced"
        );
        assert!(!root.path().join(".retired-managed").exists());

        // Replacing the link with the definition lets retirement finish.
        std::fs::remove_file(&path).expect("drop the link");
        std::fs::write(&path, &seeded).expect("restore the definition");
        let reconciled = sync(&routines_dir, ManagedAssetReconcileMode::Apply);
        assert_eq!(reconciled.retired, 1);
        assert!(!path.exists());

        // A lifecycle-edited retired default needs a preserved copy; a linked
        // `.retired-managed` would move it out of the workspace, so it stays.
        let backups = tempdir().expect("create outside backup dir");
        symlink(backups.path(), root.path().join(".retired-managed")).expect("link backups");
        let opted_in = seeded.replace("enabled: false", "enabled: true");
        std::fs::write(&path, &opted_in).expect("write the operator's copy");
        record_as_orbit_written(&routines_dir, "auto_task_scheduler", &seeded);
        let reconciled = sync(&routines_dir, ManagedAssetReconcileMode::Apply);
        assert_eq!(reconciled.retired, 0);
        assert_eq!(
            outcome_of(&reconciled, "auto_task_scheduler"),
            vec![ManagedAssetOutcome::Preserved]
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read"), opted_in);
        assert!(only_entries(backups.path()).is_empty());

        // The untracked pass refuses the same linked route.
        std::fs::remove_file(&path).expect("drop the tracked copy");
        let untracked = routines_dir.join("task_triage.yaml");
        let triage = render(retired_template("task_triage"), "task_triage", "workspace");
        std::fs::write(&untracked, &triage).expect("write an untracked retired default");
        let reconciled = sync(&routines_dir, ManagedAssetReconcileMode::Apply);
        assert_eq!(
            outcome_of(&reconciled, "task_triage"),
            vec![ManagedAssetOutcome::Preserved]
        );
        assert_eq!(std::fs::read_to_string(&untracked).expect("read"), triage);
        assert!(only_entries(backups.path()).is_empty());
    }

    /// An untracked link wearing a retired default's shape is skipped, as the
    /// loader skips it: neither read nor moved.
    #[test]
    fn untracked_link_to_a_retired_shape_is_not_retired() {
        let root = tempdir().expect("create tempdir");
        let outside = tempdir().expect("create outside dir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed current defaults");

        let seeded = render(
            retired_template("auto_task_scheduler"),
            "auto_task_scheduler",
            "workspace",
        );
        let target = outside.path().join("auto_task_scheduler.yaml");
        std::fs::write(&target, &seeded).expect("write external bytes");
        let path = routines_dir.join("auto_task_scheduler.yaml");
        symlink(&target, &path).expect("link outside");

        let reconciled = sync(&routines_dir, ManagedAssetReconcileMode::Apply);
        assert_eq!(reconciled.retired, 0);
        assert!(outcome_of(&reconciled, "auto_task_scheduler").is_empty());
        assert_is_link(&path);
        assert_eq!(std::fs::read_to_string(&target).expect("read"), seeded);
    }
}
