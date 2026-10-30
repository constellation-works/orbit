//! Conservative durable-owner builder. Missing historical facts are gaps.
use super::{DeliveryQualityPage, DeliveryQualitySource, contract::*};
use crate::OrbitRuntime;
use orbit_common::{OrbitError, storage::blob_store::BlobStore};
use orbit_store::V2AuditEventFilter;
use orbit_types::workflow::{JobRun, RunDeliveryStatus};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) struct RuntimeDeliveryQualitySource<'a>(
    pub &'a OrbitRuntime,
    pub chrono::DateTime<chrono::Utc>,
);
impl DeliveryQualitySource for RuntimeDeliveryQualitySource<'_> {
    fn delivery_quality_page(&self, after: &str) -> Result<DeliveryQualityPage, OrbitError> {
        let (after_run, after_task): (String, Option<String>) = if after.is_empty() {
            (String::new(), None)
        } else {
            serde_json::from_str(after)
                .map_err(|_| OrbitError::Store("invalid delivery scan cursor".into()))?
        };
        let runs = if after_task.is_some() {
            vec![self.0.show_job_run_observed(&after_run)?]
        } else {
            self.0.stores().jobs().observed_run_page(&after_run, 1)?
        };
        let mut next_cursor = runs
            .last()
            .map(|r| serde_json::to_string(&(r.run_id.to_string(), Option::<String>::None)))
            .transpose()
            .map_err(|_| OrbitError::Store("invalid delivery cursor".into()))?
            .unwrap_or_default();
        let mut units = Vec::new();
        let jobs = self.0.task_delivery_job_ids()?;
        for run in runs {
            if run.created_at < self.1
                || !run.state.is_terminal()
                || !jobs.contains(run.job_id.as_str())
            {
                continue;
            }
            let input = run.input.as_ref().unwrap_or(&Value::Null);
            let mut tasks: BTreeSet<String> = input
                .get("task_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if let Some(id) = input.get("task_id").and_then(Value::as_str) {
                tasks.insert(id.into());
            }
            // A batch is paged by exact task identity too. No cap silently
            // loses older tasks or blocks the owner's cursor forever.
            let remaining: Vec<_> = tasks
                .iter()
                .filter(|id| after_task.as_ref().is_none_or(|after| *id > after))
                .collect();
            if remaining.len() > 32 {
                next_cursor = serde_json::to_string(&(run.run_id.to_string(), remaining[31]))
                    .map_err(|_| OrbitError::Store("invalid delivery cursor".into()))?;
            }
            for task in remaining.into_iter().take(32) {
                let observation = self
                    .0
                    .observe_task_delivery(task, Some(run.run_id.as_str()))?;
                if !matches!(
                    observation.delivery_status,
                    RunDeliveryStatus::Landed
                        | RunDeliveryStatus::Committed
                        | RunDeliveryStatus::NoChange
                ) {
                    continue;
                }
                // The observation authenticates inherited commit ownership.
                // Use that source run, never this retry's mutable crew.
                let source_run = observation
                    .commit
                    .provenance
                    .as_ref()
                    .and_then(|p| p.source_run_id.as_deref())
                    .unwrap_or(run.run_id.as_str());
                let actual = self.0.show_job_run_observed(source_run)?;
                let mut unit = self.0.freeze_observed_delivery(&actual, task)?;
                unit.identity.base_revision = observation.commit.base_sha;
                unit.identity.candidate_revision = observation.commit.head_sha;
                unit.identity.landed_revision = observation.landing.landed_commit;
                if tasks.len() != 1 {
                    unit.provenance.attribution = DeliveryQualityAttribution::Shared;
                    unit.omissions.push("joint_task_mapping".into());
                }
                if observation.delivery_status == RunDeliveryStatus::NoChange {
                    unit.non_code = true;
                }
                self.0.hydrate_delivery_quality_checks(&mut unit, &actual)?;
                self.0.complete_delivery_quality_patch(&mut unit)?;
                units.push(unit);
            }
        }
        if units.len() > 32 {
            return Err(OrbitError::InvalidInput(
                "delivery page unit mapping exceeds bound".into(),
            ));
        }
        Ok(DeliveryQualityPage { units, next_cursor })
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn ancestor_activity(event: &Value, events: &BTreeMap<String, Value>) -> Option<String> {
    let mut current = event;
    for _ in 0..16 {
        if let Some(name) = text(current, "activity_name") {
            return Some(name);
        }
        current = events.get(current.get("parent_event_id")?.as_str()?)?;
    }
    None
}

impl OrbitRuntime {
    fn freeze_observed_delivery(
        &self,
        run: &JobRun,
        task: &str,
    ) -> Result<FrozenDeliveryQuality, OrbitError> {
        // Bounded audit read: overflow is never a complete packet.
        let rows = self.list_v2_audit_events(V2AuditEventFilter {
            run_id: Some(run.run_id.to_string()),
            source: Some("v2_envelope".into()),
            limit: Some(1025),
            ..Default::default()
        })?;
        let overflow = rows.len() > 1024;
        let events: BTreeMap<String, Value> = rows
            .into_iter()
            .take(1024)
            .filter_map(|row| {
                serde_json::from_str(&row.payload_json)
                    .ok()
                    .map(|v| (row.event_id, v))
            })
            .collect();
        let invocations: Vec<_> = events
            .values()
            .filter(|e| {
                e.get("event_type").and_then(Value::as_str) == Some("cli.invocation.started")
            })
            .collect();
        let implementations: Vec<_> = invocations
            .iter()
            .filter(|e| ancestor_activity(e, &events).as_deref() == Some("agent_implement"))
            .copied()
            .collect();
        let mut omissions = Vec::new();
        if overflow {
            omissions.push("audit_overflow".into());
        }
        let invocation = if implementations.len() == 1 {
            implementations[0]
        } else {
            &Value::Null
        };
        let blobs = BlobStore::new(self.data_root().join("state/audit/blobs"));
        let scope = invocation
            .get("task_scope_blob_ref")
            .and_then(Value::as_str)
            .and_then(|id| blobs.read_prefix(id, 65_537).ok())
            .filter(|bytes| bytes.len() <= 65_536)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|v| v.get("id").and_then(Value::as_str) == Some(task));
        let task_scope = scope.as_ref().and_then(|v| {
            Some(DeliveryQualityScope {
                title: text(v, "title")?,
                description: text(v, "description")?,
                criteria: serde_json::from_value(v.get("acceptance_criteria")?.clone()).ok()?,
            })
        });
        let contribution_refs = invocations
            .iter()
            .filter_map(|e| text(e, "event_id"))
            .collect();
        let implementation_models = self.invocation_records(orbit_store::InvocationQuery {
            workspace_id: Some(self.workspace_id()?),
            job_run_id: Some(run.run_id.to_string()),
            activity_id: Some("agent_implement".into()),
            limit: 2,
            ..Default::default()
        })?;
        let actual_model = if implementation_models.len() == 1 {
            implementation_models[0].model.clone()
        } else {
            None
        };
        let provenance = DeliveryQualityProvenance {
            actual_provider: text(invocation, "provider"),
            actual_model,
            actual_effort: text(invocation, "reasoning_effort"),
            configuration_fingerprint: text(invocation, "configuration_fingerprint"),
            actual_crew: run.resolved_crew.clone(),
            admission_tier: scope.as_ref().and_then(|s| text(s, "admission_tier")),
            admission_type: scope.as_ref().and_then(|s| text(s, "admission_type")),
            attribution: if implementations.len() == 1 && invocations.len() == 1 {
                DeliveryQualityAttribution::Available
            } else if !invocations.is_empty() {
                DeliveryQualityAttribution::Shared
            } else {
                DeliveryQualityAttribution::Unavailable
            },
            contribution_refs,
        };
        // Publication must be authenticated independently from commit creation.
        // Older pipelines without a published-head checkpoint abstain.
        let published_verified = false;
        if scope
            .as_ref()
            .and_then(|s| s.get("context_files"))
            .and_then(Value::as_array)
            .is_some_and(|selectors| {
                selectors.len() > 128
                    || selectors.iter().any(|s| {
                        s.as_str()
                            .is_none_or(|s| !(s.starts_with("file:") || s.starts_with("symbol:")))
                    })
            })
        {
            omissions.push("context_selector_unresolved".into());
        }
        let context = scope
            .as_ref()
            .and_then(|s| s.get("context_files"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|selector| {
                selector
                    .strip_prefix("file:")
                    .or_else(|| {
                        selector
                            .strip_prefix("symbol:")
                            .and_then(|s| s.split('#').next())
                    })
                    .map(|path| DeliveryQualityEvidence {
                        id: format!("context:{path}"),
                        revision: String::new(),
                        content: String::new(),
                    })
            })
            .take(128)
            .collect();
        let mut validations = Vec::new();
        if let Some(commands) = scope
            .as_ref()
            .and_then(|s| s.get("final_validation_commands"))
            .and_then(Value::as_array)
        {
            for command in commands.iter().filter_map(Value::as_str) {
                validations.push(DeliveryQualityValidation {
                    command: command.into(),
                    selection: Vec::new(),
                    revision: String::new(),
                    outcome: DeliveryQualityValidationOutcome::Unavailable,
                    evidence_ref: None,
                });
            }
        }
        let owner = self
            .workspace_owner_machine_id()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                omissions.push("owner_unavailable".into());
                "unavailable".into()
            });
        Ok(FrozenDeliveryQuality {
            identity: DeliveryQualityIdentity {
                owner,
                workspace: self.workspace_id()?,
                task_id: task.into(),
                implementation_attempt: text(invocation, "event_id")
                    .unwrap_or_else(|| format!("{}:{}", run.run_id, run.attempt)),
                actual_run_id: run.run_id.to_string(),
                base_revision: None,
                candidate_revision: None,
                landed_revision: None,
            },
            provenance,
            task_scope,
            patch: String::new(),
            changed_paths: Vec::new(),
            published_verified,
            integration_patch: None,
            guides: Vec::new(),
            context,
            validations,
            applicability: DELIVERY_QUALITY_DIMENSIONS
                .into_iter()
                .map(|d| (d.into(), DeliveryQualityApplicability::Applicable))
                .collect(),
            applicability_rule: "implemented-code/1".into(),
            non_code: false,
            omissions,
            truncated: false,
            source_version: "durable-delivery-source/1".into(),
        })
    }

    fn hydrate_delivery_quality_checks(
        &self,
        unit: &mut FrozenDeliveryQuality,
        run: &JobRun,
    ) -> Result<(), OrbitError> {
        if let Some(published) = run
            .steps
            .iter()
            .filter(|step| {
                step.target_id.as_str() == "git_push"
                    && step.state == orbit_types::workflow::JobRunState::Success
            })
            .filter_map(|step| {
                step.agent_response_json
                    .as_ref()
                    .and_then(|output| output.get("local_sha"))
                    .and_then(Value::as_str)
            })
            .find(|sha| git2::Oid::from_str(sha).is_ok())
        {
            if unit.identity.candidate_revision.as_deref() != Some(published) {
                if let Some(committed) = &unit.identity.candidate_revision {
                    unit.provenance
                        .contribution_refs
                        .push(format!("committed:{committed}"));
                }
                unit.provenance.attribution = DeliveryQualityAttribution::Partial;
                unit.omissions.push("published_candidate_changed".into());
                unit.identity.candidate_revision = Some(published.into());
            }
            unit.published_verified = true;
        }
        for step in &run.steps {
            let Some(output) = step.agent_response_json.as_ref() else {
                continue;
            };
            if step.target_id.as_str() != "candidate_validate"
                || output.get("tested_head").and_then(Value::as_str)
                    != unit.identity.candidate_revision.as_deref()
            {
                continue;
            }
            for reference in output
                .get("validation")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(128)
            {
                let Some(path) = reference.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let Some(artifact) = self.get_task_artifact_inline(&unit.identity.task_id, path)?
                else {
                    continue;
                };
                let digest = orbit_types::assistance::assistance_digest(&artifact.content);
                if reference.get("sha256").and_then(Value::as_str) != Some(&digest[7..]) {
                    unit.omissions.push("validation_digest_mismatch".into());
                    continue;
                }
                let Ok(log) = serde_json::from_slice::<Value>(&artifact.content) else {
                    continue;
                };
                if log.get("role").and_then(Value::as_str) == Some("baseline") {
                    continue;
                }
                let Some(command) = log.get("command").and_then(Value::as_str) else {
                    continue;
                };
                let selection = log
                    .get("summary")
                    .and_then(|s| s.get("selection"))
                    .filter(|v| !v.is_null())
                    .map(|v| vec![v.to_string()])
                    .unwrap_or_default();
                let outcome = match log.get("exit_code").and_then(Value::as_i64) {
                    Some(0) => DeliveryQualityValidationOutcome::Passed,
                    Some(_) => DeliveryQualityValidationOutcome::Failed,
                    None => DeliveryQualityValidationOutcome::Unavailable,
                };
                if selection.is_empty() {
                    unit.omissions
                        .push("validation_selection_unavailable".into());
                }
                let validation = DeliveryQualityValidation {
                    command: command.into(),
                    selection,
                    revision: unit.identity.candidate_revision.clone().unwrap_or_default(),
                    outcome,
                    evidence_ref: Some(format!("validation:{path}")),
                };
                if let Some(previous) = unit.validations.iter_mut().find(|v| v.command == command) {
                    *previous = validation;
                } else {
                    unit.validations.push(validation);
                }
            }
        }
        Ok(())
    }

    fn complete_delivery_quality_patch(
        &self,
        unit: &mut FrozenDeliveryQuality,
    ) -> Result<(), OrbitError> {
        let result = (|| -> Option<()> {
            let repo = git2::Repository::discover(&self.paths().repo_root).ok()?;
            let base = repo
                .find_commit(git2::Oid::from_str(unit.identity.base_revision.as_deref()?).ok()?)
                .ok()?;
            let candidate = repo
                .find_commit(
                    git2::Oid::from_str(unit.identity.candidate_revision.as_deref()?).ok()?,
                )
                .ok()?;
            let base_tree = base.tree().ok()?;
            let tree = candidate.tree().ok()?;
            let mut diff = repo
                .diff_tree_to_tree(Some(&base_tree), Some(&tree), None)
                .ok()?;
            diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))
                .ok()?;
            if diff.deltas().len() > 128 {
                unit.omissions.push("changed_path_manifest_overflow".into());
                return Some(());
            }
            for delta in diff.deltas() {
                for path in [delta.old_file().path(), delta.new_file().path()]
                    .into_iter()
                    .flatten()
                {
                    unit.changed_paths.push(path.to_str()?.into());
                }
                if delta.old_file().is_binary() || delta.new_file().is_binary() {
                    unit.omissions.push("binary_diff".into());
                }
            }
            unit.changed_paths.sort();
            unit.changed_paths.dedup();
            unit.non_code = !unit.changed_paths.is_empty()
                && unit.changed_paths.iter().all(|p| {
                    matches!(
                        Path::new(p).extension().and_then(|s| s.to_str()),
                        Some("md" | "rst" | "txt" | "adoc")
                    )
                });

            let mut bytes = Vec::new();
            let printed = diff.print(git2::DiffFormat::Patch, |_, _, line| {
                if bytes.len() + line.content().len() > 48_000 {
                    return false;
                }
                if matches!(line.origin(), '+' | '-' | ' ') {
                    bytes.push(line.origin() as u8);
                }
                bytes.extend_from_slice(line.content());
                true
            });
            if printed.is_err() {
                unit.truncated = true;
                unit.omissions.push("oversize_diff".into());
            }
            unit.patch = String::from_utf8(bytes).ok()?;
            for path in ["CLAUDE.md", "ARCHITECTURE.md"] {
                match tree
                    .get_path(Path::new(path))
                    .ok()
                    .and_then(|e| repo.find_blob(e.id()).ok())
                    .filter(|b| b.size() <= 8192)
                {
                    Some(blob) => unit.guides.push(DeliveryQualityEvidence {
                        id: format!("guide:{path}"),
                        revision: candidate.id().to_string(),
                        content: String::from_utf8_lossy(blob.content()).into_owned(),
                    }),
                    None => unit.omissions.push(format!("guide_unavailable:{path}")),
                }
            }
            // Freeze all host-declared file/symbol context and both sides of
            // the changed files. An absent or oversized member is an omission.
            for path in &unit.changed_paths {
                unit.context.push(DeliveryQualityEvidence {
                    id: format!("context:{path}"),
                    revision: String::new(),
                    content: String::new(),
                });
            }
            unit.context.sort_by(|a, b| a.id.cmp(&b.id));
            unit.context.dedup_by(|a, b| a.id == b.id);
            let mut context = Vec::new();
            let mut context_bytes = 0;
            for selected in &unit.context {
                let path = selected.id.strip_prefix("context:")?;
                for (revision, source_tree) in [(base.id(), &base_tree), (candidate.id(), &tree)] {
                    let Some(entry) = source_tree.get_path(Path::new(path)).ok() else {
                        continue;
                    };
                    let Some(blob) = repo
                        .find_blob(entry.id())
                        .ok()
                        .filter(|b| b.size() <= 8192 && context_bytes + b.size() <= 16_384)
                    else {
                        unit.omissions.push("context_size_or_object_gap".into());
                        continue;
                    };
                    context_bytes += blob.size();
                    context.push(DeliveryQualityEvidence {
                        id: format!("context:{}:{path}", revision),
                        revision: revision.to_string(),
                        content: String::from_utf8_lossy(blob.content()).into_owned(),
                    });
                }
            }
            unit.context = context;
            if let Some(landed) = &unit.identity.landed_revision
                && landed != &candidate.id().to_string()
            {
                unit.provenance.attribution = DeliveryQualityAttribution::Partial;
                unit.omissions
                    .push("integration_contributions_unmapped".into());
            }
            Some(())
        })();
        if result.is_none() {
            unit.omissions.push("git_objects_unavailable".into());
        }
        Ok(())
    }
}
