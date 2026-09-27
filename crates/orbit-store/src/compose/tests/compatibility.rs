use crate::Store;
use crate::driver::sqlite::migration::SUPPORTED_SCHEMA_VERSION;
use crate::driver::sqlite::{automation, job_run_store, review};
use crate::workflow::layout::SUPPORTED_LAYOUT_VERSION;

use super::super::compiled_compatibility;

#[test]
fn the_compiled_identity_names_the_ledger_versions_this_build_migrates_to() {
    let identity = compiled_compatibility();
    assert_eq!(identity.store_schema.version, SUPPORTED_SCHEMA_VERSION);
    assert_eq!(identity.workspace_layout.version, SUPPORTED_LAYOUT_VERSION);
    for ledger in [identity.store_schema, identity.workspace_layout] {
        // A migration that breaks older readers breaks their writes too.
        assert!(
            ledger.reader_floor <= ledger.writer_floor && ledger.writer_floor <= ledger.version,
            "floors must be ordered within the ledger: {ledger:?}"
        );
    }
}

#[test]
fn the_compiled_identity_names_every_feature_schema_at_its_migrated_version() {
    let store = Store::open_in_memory().expect("store");
    let identity = compiled_compatibility();
    let registries = [
        (automation::FEATURE, automation::MIGRATIONS),
        (review::FEATURE, review::MIGRATIONS),
        (
            job_run_store::pull::FEATURE,
            job_run_store::pull::MIGRATIONS,
        ),
    ];
    assert_eq!(identity.features.len(), registries.len(), "{identity}");
    for (feature, migrations) in registries {
        store
            .apply_feature_migrations(feature, migrations)
            .expect("apply feature schema");
        let status = store
            .feature_schema_status(feature, migrations)
            .expect("feature status");
        assert_eq!(
            identity.features.get(feature),
            Some(&status.current_version),
            "{feature}"
        );
    }
}
