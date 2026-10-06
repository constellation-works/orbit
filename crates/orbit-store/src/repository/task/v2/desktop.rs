//! Desktop mutations share the existing task bundle journal and commit point.
use super::*;
use crate::contracts::{AtomicTaskMutationOutcome, DesktopTaskMutationParams};
use crate::driver::file::task_bundle::PendingWriteGuard;

fn revision(bundle: &TaskBundleV2) -> Result<String, OrbitError> {
    let bytes = serde_json::to_vec(&(
        &bundle.envelope,
        &bundle.description,
        &bundle.acceptance,
        &bundle.plan,
        &bundle.execution_summary,
        &bundle.events,
        &bundle.comments,
        &bundle.artifact_manifest,
    ))
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(sha256_hex(&bytes))
}
/// Why an active claim refuses desktop writes, and the supported way out: a
/// claim-scoped mutation, never a status change.
fn active_claim_reason(phase: crate::contracts::ExecutionClaimPhase) -> &'static str {
    match phase {
        crate::contracts::ExecutionClaimPhase::HandedOff => {
            "active execution claim requires a claim-scoped mutation: its handoff awaits the \
             owner's completion authority; land it through that authority, or revoke the \
             handoff and recover the claim from the owner's operator console"
        }
        crate::contracts::ExecutionClaimPhase::RepairPending => {
            "active execution claim requires a claim-scoped mutation: its landing stopped on \
             its base and an automatic repair is pending; let the repair re-hand it off, or \
             recover the claim from the owner's operator console"
        }
        _ => {
            "active execution claim requires a claim-scoped mutation: its run is still \
             executing; wait for it to settle, or recover the claim from the owner's operator \
             console"
        }
    }
}

impl TaskV2Store {
    pub(crate) fn read_desktop_task(
        &self,
        id: &str,
    ) -> Result<crate::contracts::DesktopTaskRead, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.ensure_recovered()?;
        let bundle = self.read_existing_bundle(id)?;
        let revision = revision(&bundle)?;
        let comments = bundle
            .comments
            .iter()
            .map(|comment| TaskComment {
                at: comment.at,
                by: comment.by.clone(),
                message: comment.body.clone(),
            })
            .collect();
        let history = super::sidecars::task_history_from_events(bundle.events.clone());
        let artifacts = bundle
            .artifact_manifest
            .as_ref()
            .map(|manifest| manifest.files.clone())
            .unwrap_or_default();
        let task = self.task_from_bundle(bundle)?;
        let readonly = self.registry.is_read_only()?
            || fs::metadata(self.bundle_store.bundle_path(id)?)?
                .permissions()
                .readonly();
        let write_disabled_reason = if readonly {
            Some("task storage is read-only".into())
        } else if let Some(boundary) = &self.coordination {
            match boundary.inspect_execution_claims() {
                Ok(claims) => claims
                    .iter()
                    .find(|claim| {
                        claim.claim.task_id == id && claim.claim.phase.protects_footprint()
                    })
                    .map(|claim| active_claim_reason(claim.claim.phase).into()),
                Err(error) => Some(error.to_string()),
            }
        } else {
            None
        };
        Ok(crate::contracts::DesktopTaskRead {
            task,
            revision,
            comments,
            history,
            artifacts,
            write_disabled_reason,
        })
    }

    pub(crate) fn desktop_task_revision(&self, id: &str) -> Result<String, OrbitError> {
        self.ensure_recovered()?;
        revision(&self.read_existing_bundle(id)?)
    }
    pub(crate) fn apply_desktop_task_mutation(
        &self,
        id: &str,
        p: &DesktopTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if self.registry.is_read_only()?
            || fs::metadata(self.bundle_store.bundle_path(id)?)?
                .permissions()
                .readonly()
        {
            return Err(OrbitError::Store(
                "desktop task storage is read-only".into(),
            ));
        }
        if p.actor.trim().is_empty()
            || p.request_id.is_empty()
            || p.request_id.len() > 128
            || !p
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || p.payload_digest.len() != 64
        {
            return Err(OrbitError::InvalidInput(
                "invalid desktop request identity".into(),
            ));
        }
        self.with_task_lock(id, || {
            let mut b = self.read_existing_bundle(id)?;
            let key = format!("desktop_request={} ", p.request_id);
            let receipt = format!("{key}digest={}", p.payload_digest);
            if let Some(event) = b.events.iter().find(|e| {
                e.event_type == "desktop_mutation"
                    && e.note.as_deref().is_some_and(|n| n.starts_with(&key))
            }) {
                if event.note.as_deref() != Some(receipt.as_str()) {
                    return Err(OrbitError::InvalidInput(
                        "request identity reused with a different payload".into(),
                    ));
                }
                return Ok(AtomicTaskMutationOutcome::AlreadyApplied);
            }
            if revision(&b)? != p.expected_revision {
                return Ok(AtomicTaskMutationOutcome::Stale);
            }
            let old_status = b.envelope.status;
            if let Some(v) = &p.fields.title {
                b.envelope.title = v.clone();
            }
            if let Some(v) = p.fields.priority {
                b.envelope.priority = v;
            }
            if let Some(v) = &p.fields.crew {
                b.envelope.crew = (!v.is_empty()).then(|| v.clone());
            }
            if let Some(v) = p.status {
                b.envelope.status = v;
            }
            if let Some(boundary) = &self.coordination {
                boundary.guard_ordinary_footprint(b.envelope.status, &b.envelope.context_files)?;
            }
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            if let Some(v) = &p.fields.description {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::Description, v)?;
            }
            if let Some(v) = &p.fields.acceptance_criteria {
                self.bundle_store.rewrite_document(
                    id,
                    TaskDocumentV2::Acceptance,
                    &render_acceptance(v),
                )?;
            }
            let now = Utc::now();
            if let Some(v) = &p.comment {
                self.bundle_store.append_comment(
                    id,
                    &TaskCommentRowV2 {
                        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                        comment_id: format!("C-{:04}", next_sequence(&b.comments, "C-")),
                        at: now,
                        by: p.actor.clone(),
                        body: v.clone(),
                    },
                )?;
            }
            let approval =
                old_status == TaskStatus::Proposed && b.envelope.status == TaskStatus::Backlog;
            if approval {
                let approval = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: next_event_id(&b.events),
                    at: now,
                    by: p.actor.clone(),
                    event_type: "proposal_approved".into(),
                    note: None,
                    from_status: Some(old_status),
                    to_status: Some(b.envelope.status),
                };
                self.bundle_store.append_event(id, &approval)?;
                b.events.push(approval);
            }
            let event = TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: next_event_id(&b.events),
                at: now,
                by: p.actor.clone(),
                event_type: "desktop_mutation".into(),
                note: Some(receipt),
                from_status: (!approval && old_status != b.envelope.status).then_some(old_status),
                to_status: (!approval && old_status != b.envelope.status)
                    .then_some(b.envelope.status),
            };
            self.bundle_store.append_event(id, &event)?;
            b.events.push(event);
            let context_files = b.envelope.context_files.clone();
            append_creation_grant(
                &self.bundle_store,
                &mut b,
                &context_files,
                &[],
                &p.actor,
                now,
            )?;
            b.envelope.updated_at = now;
            self.bundle_store.rewrite_envelope(id, &b.envelope)?;
            pending.finish();
            self.replace_index_best_effort(&b.envelope, "desktop task mutation");
            Ok(AtomicTaskMutationOutcome::Applied)
        })
    }
}
