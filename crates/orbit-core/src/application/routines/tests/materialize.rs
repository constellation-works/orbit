//! Managed-routine writes and retirement stay confined to the catalog.

use orbit_common::security::release::sha256_hex;

use super::super::seed::{
    BASE_BRANCH_PLACEHOLDER, DEFAULT_ROUTINE_FILES, OWNER_MACHINE_PLACEHOLDER,
    RETIRED_ROUTINE_FILES,
};
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

fn retired_template(stem: &str) -> &'static str {
    RETIRED_ROUTINE_FILES
        .iter()
        .find(|(name, _)| *name == stem)
        .map(|(_, template)| *template)
        .expect("retired template is shipped as a provenance shape")
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
    use super::super::super::seed::RoutineSeedIdentity;
    use super::{
        current_template, manifest_tracks, outcome_of, record_as_orbit_written, render,
        retired_template,
    };
    use crate::application::managed_assets::{
        ManagedAssetOutcome, ManagedAssetReconcileMode, ManagedAssetReconciliation,
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
}
