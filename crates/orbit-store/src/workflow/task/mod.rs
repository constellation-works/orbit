//! Task-migration and publication tooling: export a workspace's task bundles
//! to a portable tar.zst archive, import them into another machine's registry,
//! build validated publication snapshots, publish them to a dedicated
//! publication repository, and inspect that repository as labelled read-only
//! state.
//!
//! The engine lives in `orbit-store` because it needs the canonical bundle I/O
//! primitives ([`read_bundle_at`]/[`write_bundle_at`]) and the private
//! [`TaskRegistryStore`] internals. `orbit-core` exposes thin facades over it;
//! `orbit-cli` wires the `orbit task export/import/reindex` surfaces.
//!
//! # Model
//! - Canonical bundles live at `<global>/tasks/workspaces/<ws-id>/<ORB-xxxxx>/`.
//! - Task ids are a *global* primary key in `index.sqlite` and the allocator has
//!   a single `local` authority, so merging two machines' tasks collides on ids.
//! - Export copies bundle trees verbatim plus a [`TaskMigrationManifest`].
//! - Import validates everything *before* mutating state, then keeps free ids,
//!   renumbers collisions (rewriting relation targets within the imported set),
//!   rebuilds index rows from bundle YAML, bumps the allocator past the max
//!   landed id. Fresh bundles and a workspace registration created by a failed
//!   run are rolled back; owner-wins replacements remain because the incoming
//!   owner's copy is authoritative.
//! - Idempotency is scoped to *kept* ids: re-importing an archive whose ids are
//!   free (or already landed unchanged) is a no-op. A `--on-conflict=renumber`
//!   run is **not** idempotent — a collision means "these are new local tasks,"
//!   so each run mints fresh ids. Import once; the printed `.idmap.json` records
//!   what landed.
//!
//! # Owner-wins sync
//! Task authority follows the id prefix: the host that minted `ORB-*` is the
//! sole writer of those tasks, and any copy on a host that mints `DANI-*` is a
//! read-only mirror. [`ImportConflictPolicy::OwnerWins`] makes import a
//! repeatable sync of those mirrors — a colliding foreign-prefix id is replaced
//! by the owner's bundle ([`ImportAction::Updated`]), a colliding local-prefix
//! id is left alone ([`ImportAction::SkippedLocalOwned`]), and nothing is ever
//! renumbered. An owner-wins import also repairs an orphaned *foreign-prefix*
//! canonical bundle whose registry binding is missing by replacing it and
//! restoring the binding; an orphaned local-prefix bundle stays local-owned.
//! Foreign relation targets therefore survive verbatim, no `.idmap.json` is
//! written, and a second run of the same archive reports every task as
//! already-present.
//!
//! # Artifact blobs
//! Bundles carry a `artifacts/manifest.yaml` sidecar and the referenced blobs
//! under `artifacts/files/**`. Export tars the entire canonical bundle tree, so
//! blobs are always present in the archive. Import validates each blob against
//! the manifest during staging ([`read_bundle_at`] hashes every file), then
//! publishes the bundle and blob tree together with
//! [`write_bundle_with_artifacts_at`] under the same rollback guard.
//!
//! ## Backfilling stranded bundles
//! Before ORB-10042, `write_bundle_at` wrote only the manifest, so any
//! successfully-imported artifact bundle landed with `artifacts/files/` empty
//! and later fails `read_bundle_at`. `orbit task artifact put` refuses tasks in
//! `done` status, so those closed tasks have no in-CLI repair path. To backfill,
//! keep the source archive that produced the stranded bundles and run:
//!
//! ```text
//! # 1. Extract the archive next to your workspace's canonical tasks tree.
//! tar --use-compress-program=unzstd -xf tasks.tar.zst -C /tmp/orbit-backfill
//!
//! # 2. For each stranded ORB-id, copy the blob tree onto the canonical bundle.
//! rsync -av /tmp/orbit-backfill/bundles/ORB-XXXXX/artifacts/files/ \
//!   <global>/tasks/workspaces/<ws-id>/ORB-XXXXX/artifacts/files/
//!
//! # 3. Re-index to validate the restored bundles end-to-end.
//! orbit task reindex --task-workspace <task-workspace-id>
//! ```
//!
//! The manifest hashes are the source of truth — `orbit task reindex` reads
//! every bundle (recovering an incomplete pending write), recomputes each
//! blob's SHA-256, and surfaces any file whose bytes don't match the
//! manifest. If the archive is gone, the bundle is unrecoverable from this
//! side (regenerate the artifact upstream and paste it back at the recorded
//! `blob` path with matching bytes).

mod archive;
mod export;
mod git;
mod import;
mod inspect;
mod manifest;
mod publication;
mod publish;
mod reindex;
mod restore;

#[cfg(test)]
mod tests;

pub use export::{ExportOutcome, ExportSelection, export_tasks};
pub use import::{ImportAction, ImportConflictPolicy, ImportOutcome, ImportedTask, import_tasks};
pub use inspect::{
    InspectedPublicationTask, PublicationFreshness, PublicationInspectLabel,
    PublicationInspectRequest, PublicationInspection, PublicationRenderAuthority,
    inspect_publication,
};
pub use manifest::{MIGRATION_FORMAT_VERSION, TaskMigrationManifest};
pub use publication::{
    AttachmentPolicy, AttachmentPolicyKind, AttachmentScanFailure, AttachmentScanInput,
    AttachmentScanOutcome, AttachmentSensitivityScanner, OmittedAttachment,
    PUBLICATION_ENVELOPE_FILE_NAME, PUBLICATION_TASKS_DIR_NAME, PublicationEnvelope,
    PublicationSnapshotMetadata, PublicationSnapshotOutcome, ScannerFailureBehavior,
    TASK_PUBLICATION_FORMAT_VERSION, build_publication_snapshot,
};
pub use publish::{
    PublicationCallerRole, PublicationLastSuccess, PublicationPublishOutcome,
    PublicationPublishRequest, PublicationPublishStatus, publish_task_snapshot,
};
pub use reindex::{ReindexOutcome, reindex_workspace};
pub use restore::{
    PublicationRecoveryCompleteness, PublicationRestoreMode, PublicationRestoreOutcome,
    PublicationRestoreRequest, restore_publication,
};
