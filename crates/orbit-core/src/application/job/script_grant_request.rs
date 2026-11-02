//! `script_grant` decision requests (script steps §2 route 3, phase 4).
//!
//! When a script step refuses because its grant is missing, stale or
//! narrower than the manifest asks, Orbit raises one `script_grant` request
//! instead of leaving the operator to discover a failed run. The request
//! binds the workspace, script name, content digest and requested
//! capability set, and carries the diff against the last granted bytes.
//! Raising never grants. Requests live in the history of one `someday`
//! holder task per script (tagged [`SCRIPT_GRANT_TASK_TAG`]), which no
//! agent dispatch picks up, so they reach every decision surface (the
//! decision list, the dashboard, registered channels' digests) unchanged.
//!
//! Only a human answers one: the operator through `orbit decision answer`
//! or the dashboard, or a registered decision channel's
//! `orbit.decision.submit`, all of which land in
//! `answer_decision_request_as_human`. A `grant` answer writes the grant at
//! exactly the bound digest and capability set, with the answering human as
//! grantor and the surface (`decision-channel <name>` for a channel) as
//! provenance; a script whose bytes or requests moved on since the request
//! was raised is refused as superseded.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_engine::activity_job::ScriptSnapshot;
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DecisionBinding, DecisionGate, DecisionRequest, DecisionRequestState, SCRIPT_GRANT_GRANT,
    SCRIPT_GRANT_REJECT, Task, TaskStatus,
};
use orbit_types::workflow::activity_job::{
    SCRIPT_GRANT_TASK_TAG, ScriptGrantCapabilities, ScriptGrantRequest, ScriptHostRole,
    script_grant_diff,
};

use crate::OrbitRuntime;
use crate::application::task::{
    SYSTEM_ACTOR_LABEL, TaskAddParams, TaskRecordUpdateParams, TaskRecords,
};
use crate::runtime::scripts::ScriptGrantOptions;

/// The `raised_by` mechanism of a script grant request.
const SCRIPT_GRANT_MECHANISM: &str = "script_grant";

/// The title of a script's holder task; with the tag it identifies it.
fn holder_title(script: &str) -> String {
    format!("Script grant requests: `{script}`")
}

fn holder_open(task: &Task) -> bool {
    !matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    )
}

/// The grant provenance an answer surface records: the channel's name for a
/// decision channel (`decision-channel <name>`), otherwise the surface.
fn grant_provenance(via: &str) -> String {
    let mut words = via.split_whitespace();
    match (words.next(), words.next()) {
        (Some("decision-channel"), Some(name)) => format!("decision-channel {name}"),
        _ => via.to_string(),
    }
}

/// The request a refusal resolved to.
#[derive(Debug, Clone)]
pub struct ScriptGrantRaised {
    pub request: DecisionRequest,
    /// Whether the request already existed, so nothing was written.
    pub deduplicated: bool,
}

impl OrbitRuntime {
    /// Raise, or reuse, the `script_grant` request for a step refused with
    /// `refusal` over `snapshot`. A request is reused when it binds the same
    /// script, digest and capability set and is open, was rejected, or was
    /// granted and that grant still stands; otherwise one is raised, which
    /// supersedes any older request for the script.
    pub(crate) fn raise_script_grant_request(
        &self,
        snapshot: &ScriptSnapshot,
        refusal: &str,
        refusal_message: &str,
        run_id: Option<&str>,
        step_id: Option<&str>,
    ) -> Result<ScriptGrantRaised, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let asset = &snapshot.asset;
        let name = asset.name.clone();
        let granted = self.verified_script_grant(&name).ok().flatten();
        let host_role = granted
            .as_ref()
            .map_or(ScriptHostRole::Owner, |grant| grant.host_role);
        let requested = ScriptGrantCapabilities::requested_by(&asset.spec, host_role);
        let binding = DecisionBinding::ScriptGrant {
            script: name.clone(),
            digest: asset.digest.clone(),
            capabilities: requested.key(),
        };
        let lock = self.paths().orbit_dir.join("script-grant-raise");
        let mut outcome = None;
        orbit_common::fs::io::with_exclusive_file_lock(
            &lock,
            "script grant raise",
            || -> Result<(), OrbitError> {
                let holder = self.script_grant_holder(&name, true)?.ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "no holder task for script `{name}`'s grant requests"
                    ))
                })?;
                self.stores()
                    .tasks()
                    .with_task_write_lock(&holder.id, &mut || {
                        let existing = self
                            .get_task_decision_requests(&holder.id)?
                            .into_iter()
                            .rev()
                            .find(|request| {
                                request.gate == DecisionGate::ScriptGrant
                                    && request.binding == binding
                                    && match request.state {
                                        DecisionRequestState::Open => true,
                                        DecisionRequestState::Answered => {
                                            request.resolution.as_ref().is_some_and(|answer| {
                                                answer.answer_id == SCRIPT_GRANT_REJECT
                                            }) || granted
                                                .as_ref()
                                                .is_some_and(|grant| grant.digest == asset.digest)
                                        }
                                        _ => false,
                                    }
                            });
                        if let Some(existing) = existing {
                            outcome = Some(ScriptGrantRaised {
                                request: existing,
                                deduplicated: true,
                            });
                            return Ok(());
                        }
                        let granted_bytes = self.granted_script_bytes(&name).ok().flatten();
                        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
                        let granted_text = granted_bytes
                            .as_ref()
                            .map(|bytes| (text(&bytes.manifest), text(&bytes.entry)));
                        let manifest_label = format!(".orbit/resources/scripts/{name}.yaml");
                        let (diff, diff_truncated) = script_grant_diff(
                            &manifest_label,
                            granted_text.as_ref().map(|(manifest, _)| manifest.as_str()),
                            &text(&snapshot.manifest_bytes),
                            &asset.spec.entry,
                            granted_text.as_ref().map(|(_, entry)| entry.as_str()),
                            &text(&snapshot.entry_bytes),
                        );
                        let summary = match &granted {
                            None => format!(
                                "Script `{name}` has no grant on this host and a job step needs it. Grant it at digest {}?",
                                short(&asset.digest)
                            ),
                            Some(grant) if grant.digest != asset.digest => format!(
                                "Script `{name}` changed since it was granted (digest {} → {}). Review the diff and grant the new bytes?",
                                short(&grant.digest),
                                short(&asset.digest)
                            ),
                            Some(_) => format!(
                                "Script `{name}` asks for more than its grant holds. Grant the requested capabilities at digest {}?",
                                short(&asset.digest)
                            ),
                        };
                        let request = DecisionRequest::new(
                            holder.id.clone(),
                            DecisionGate::ScriptGrant,
                            DecisionGate::script_grant_answers(),
                            binding.clone(),
                            SCRIPT_GRANT_MECHANISM.to_string(),
                            chrono::Utc::now(),
                        )
                        .with_summary(summary, None)
                        .with_topic_and_filer(None, SYSTEM_ACTOR_LABEL.to_string())
                        .with_category(&self.workspace_id()?)
                        .with_script_grant(ScriptGrantRequest {
                            workspace_id: self.workspace_id()?,
                            name: name.clone(),
                            digest: asset.digest.clone(),
                            manifest_digest: asset.manifest_digest.clone(),
                            entry_digest: asset.entry_digest.clone(),
                            entry: asset.spec.entry.clone(),
                            refusal: refusal.to_string(),
                            refusal_message: refusal_message.to_string(),
                            granted_digest: granted.as_ref().map(|grant| grant.digest.clone()),
                            granted_grant_id: granted
                                .as_ref()
                                .map(|grant| grant.grant_id.clone()),
                            requested: requested.clone(),
                            granted: granted.as_ref().map(ScriptGrantCapabilities::of_grant),
                            diff,
                            diff_truncated,
                            run_id: run_id.map(str::to_string),
                            step_id: step_id.map(str::to_string),
                        });
                        self.stores().task_records().update(
                            &holder.id,
                            TaskRecordUpdateParams {
                                actor: SYSTEM_ACTOR_LABEL.to_string(),
                                append_history: vec![request.history_entry(SYSTEM_ACTOR_LABEL)],
                                ..Default::default()
                            },
                        )?;
                        let stored = self
                            .get_task_decision_requests(&holder.id)?
                            .into_iter()
                            .find(|candidate| candidate.id == request.id)
                            .unwrap_or(request);
                        outcome = Some(ScriptGrantRaised {
                            request: stored,
                            deduplicated: false,
                        });
                        Ok(())
                    })
            },
        )?;
        outcome.ok_or_else(|| {
            OrbitError::Execution(format!(
                "the grant request for script `{name}` was not recorded"
            ))
        })
    }

    /// The open holder task for `script`'s grant requests, filed as a
    /// `someday` task when `create` is set and none exists.
    fn script_grant_holder(&self, script: &str, create: bool) -> Result<Option<Task>, OrbitError> {
        let title = holder_title(script);
        let found = self.list_task_metadata()?.into_iter().find(|task| {
            task.title == title
                && task.tags.iter().any(|tag| tag == SCRIPT_GRANT_TASK_TAG)
                && holder_open(task)
        });
        if found.is_some() || !create {
            return Ok(found);
        }
        let task = self.add_task(TaskAddParams {
            title,
            description: format!(
                "Holds the `script_grant` decision requests Orbit raises when a job step refuses script `{script}` for a missing, stale or insufficient grant. \
                 Each request names the digest, the requested and granted capabilities and the diff against the last granted bytes. \
                 Answer it with `orbit decision answer <id> grant`, from the dashboard, or through a registered decision channel; \
                 `orbit script grant {script}` on the host grants directly. Raising a request never grants. \
                 This task is `someday` so no agent picks it up."
            ),
            acceptance_criteria: vec![format!(
                "Every open script_grant request for `{script}` is answered by a human or superseded by a newer digest"
            )],
            tags: vec![SCRIPT_GRANT_TASK_TAG.to_string(), "script-steps".to_string()],
            status: Some(TaskStatus::Someday),
            system_created: true,
            ..TaskAddParams::default()
        })?;
        Ok(Some(task))
    }

    /// The open `script_grant` requests in this workspace, newest last.
    pub fn open_script_grant_requests(&self) -> Result<Vec<DecisionRequest>, OrbitError> {
        self.list_decision_requests(Some(DecisionGate::ScriptGrant), None, false)
    }

    /// The gate's write for an allowed answer to a `script_grant` request,
    /// as the human `actor_label` arriving through `via`. The caller holds
    /// the holder task's write lock and has refused stale and disallowed
    /// answers. `reject` writes nothing; `grant` writes exactly the bound
    /// grant, or refuses when the script's bytes or requests moved on.
    pub(crate) fn apply_script_grant_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        actor_label: &str,
        via: &str,
    ) -> Result<(), OrbitError> {
        match answer_id {
            SCRIPT_GRANT_REJECT => return Ok(()),
            SCRIPT_GRANT_GRANT => {}
            other => {
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::AnswerNotAllowed,
                    format!("`{other}` is not a script grant answer; nothing was applied"),
                ));
            }
        }
        if is_automation_actor(actor_label) {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::CallerNotAllowed,
                format!("a script grant is the human's; `{actor_label}` is automation"),
            ));
        }
        let not_applicable = |why: String| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!("{why}; nothing was granted and the request stays open"),
            )
        };
        let superseded = |why: String| {
            OrbitError::decision_refused(
                DecisionRefusalCode::RequestSuperseded,
                format!(
                    "{why} since decision request `{}` was raised; nothing was granted. The next refusal of the step raises a request for the current bytes",
                    request.id
                ),
            )
        };
        let payload = request.script_grant.as_ref().ok_or_else(|| {
            not_applicable(format!(
                "decision request `{}` carries no script grant",
                request.id
            ))
        })?;
        let DecisionBinding::ScriptGrant {
            script,
            digest,
            capabilities,
        } = &request.binding
        else {
            return Err(not_applicable(format!(
                "decision request `{}` is not bound to a script digest",
                request.id
            )));
        };
        if payload.name != *script || payload.digest != *digest {
            return Err(not_applicable(format!(
                "decision request `{}` names a different script than it is bound to",
                request.id
            )));
        }
        if payload.workspace_id != self.workspace_id()? {
            return Err(not_applicable(format!(
                "decision request `{}` was raised for workspace {}",
                request.id, payload.workspace_id
            )));
        }
        let catalog = self.v2_script_catalog()?;
        let Some(asset) = catalog.get(script) else {
            return Err(superseded(format!("script `{script}` was removed")));
        };
        if asset.digest != *digest {
            return Err(superseded(format!(
                "script `{script}` changed (bound digest {}, current digest {})",
                short(digest),
                short(&asset.digest)
            )));
        }
        let current =
            ScriptGrantCapabilities::requested_by(&asset.spec, payload.requested.host_role);
        if current.key() != *capabilities || current != payload.requested {
            return Err(superseded(format!(
                "script `{script}`'s requested capabilities changed"
            )));
        }
        let requested = &payload.requested;
        self.grant_script(
            script,
            &ScriptGrantOptions {
                network: Some(requested.network),
                follower: requested.host_role == ScriptHostRole::Follower,
                timeout_seconds: requested.timeout_seconds,
                output_max_files: requested.output_max_files,
                output_max_file_bytes: requested.output_max_file_bytes,
                grantor: Some(actor_label.to_string()),
                provenance: Some(grant_provenance(via)),
                expected_digest: Some(digest.clone()),
                ..Default::default()
            },
        )
        .map_err(|error| match error {
            OrbitError::InvalidInput(message) => not_applicable(message),
            other => other,
        })?;
        Ok(())
    }
}

/// One script as the dashboard Scripts view shows it: grant state, the
/// change since the grant, the requested against the granted capabilities,
/// and the open grant request, if any.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScriptGrantOverview {
    pub name: String,
    /// `granted`, `stale`, `ungranted`, `revoked` (ungranted, but an earlier
    /// attempt ran under a grant), `orphaned` or `witness_refused`.
    pub state: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<orbit_types::workflow::activity_job::ScriptGrant>,
    /// The latest commit touching the manifest or entry, for a stale grant
    /// in a Git checkout: `<short sha> <subject>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<ScriptGrantCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<ScriptGrantCapabilities>,
    /// The current bytes against the last granted bytes.
    pub diff: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub diff_truncated: bool,
    /// The open `script_grant` request for the current digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_request: Option<DecisionRequest>,
}

impl OrbitRuntime {
    /// Every script with its grant standing, live diff and open request, for
    /// the dashboard Scripts view.
    pub fn script_grant_overview(&self) -> Result<Vec<ScriptGrantOverview>, OrbitError> {
        use crate::runtime::scripts::ScriptGrantState;
        let requests = self.open_script_grant_requests().unwrap_or_default();
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        let mut out = Vec::new();
        for status in self.script_statuses()? {
            let name = status.name.clone();
            let verified_grant = matches!(
                status.state,
                ScriptGrantState::Granted | ScriptGrantState::Stale
            );
            let mut state = status.state.as_str();
            if status.state == ScriptGrantState::Ungranted
                && self
                    .script_execution_history(&name, 20)
                    .unwrap_or_default()
                    .iter()
                    .any(|execution| execution.record.grant_id.is_some())
            {
                state = "revoked";
            }
            let host_role = status
                .grant
                .as_ref()
                .map_or(ScriptHostRole::Owner, |grant| grant.host_role);
            let (mut diff, mut diff_truncated) = (String::new(), false);
            let mut stale_commit = None;
            if let Some(asset) = &status.asset {
                let manifest = std::fs::read(&asset.manifest_path).unwrap_or_default();
                let entry = std::fs::read(&asset.entry_path).unwrap_or_default();
                let granted = if verified_grant {
                    self.granted_script_bytes(&name).ok().flatten()
                } else {
                    None
                };
                let granted = granted.map(|bytes| (text(&bytes.manifest), text(&bytes.entry)));
                (diff, diff_truncated) = script_grant_diff(
                    &format!(".orbit/resources/scripts/{name}.yaml"),
                    granted.as_ref().map(|(manifest, _)| manifest.as_str()),
                    &text(&manifest),
                    &asset.spec.entry,
                    granted.as_ref().map(|(_, entry)| entry.as_str()),
                    &text(&entry),
                );
                if status.state == ScriptGrantState::Stale {
                    stale_commit = self.script_change_commit(asset);
                }
            }
            let current_digest = status.asset.as_ref().map(|asset| asset.digest.clone());
            let open_request = requests
                .iter()
                .rev()
                .find(|request| {
                    request.script_grant.as_ref().is_some_and(|grant| {
                        grant.name == name && Some(&grant.digest) == current_digest.as_ref()
                    })
                })
                .cloned();
            out.push(ScriptGrantOverview {
                state,
                detail: status.detail.clone(),
                entry: status.asset.as_ref().map(|asset| asset.spec.entry.clone()),
                granted_digest: status.grant.as_ref().map(|grant| grant.digest.clone()),
                requested: status
                    .asset
                    .as_ref()
                    .map(|asset| ScriptGrantCapabilities::requested_by(&asset.spec, host_role)),
                granted: status
                    .grant
                    .as_ref()
                    .filter(|_| verified_grant)
                    .map(ScriptGrantCapabilities::of_grant),
                grant: status.grant.clone(),
                current_digest,
                stale_commit,
                diff,
                diff_truncated,
                open_request,
                name,
            });
        }
        Ok(out)
    }

    /// The latest commit that touched a script's manifest or entry, when
    /// the workspace is a Git checkout.
    fn script_change_commit(
        &self,
        asset: &orbit_engine::activity_job::ScriptAsset,
    ) -> Option<String> {
        // Most commits walked back from HEAD; an older change is not named.
        const MAX_COMMITS: usize = 500;
        let root = self.paths().repo_root.clone();
        let paths = [
            asset.manifest_path.strip_prefix(&root).ok()?.to_path_buf(),
            asset.entry_path.strip_prefix(&root).ok()?.to_path_buf(),
        ];
        let repo = git2::Repository::open(&root).ok()?;
        let mut walk = repo.revwalk().ok()?;
        walk.push_head().ok()?;
        let blob = |tree: Option<&git2::Tree<'_>>, path: &std::path::Path| {
            tree.and_then(|tree| tree.get_path(path).ok())
                .map(|entry| entry.id())
        };
        for oid in walk.take(MAX_COMMITS) {
            let commit = repo.find_commit(oid.ok()?).ok()?;
            let tree = commit.tree().ok();
            let parent = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
            if paths
                .iter()
                .any(|path| blob(tree.as_ref(), path) != blob(parent.as_ref(), path))
            {
                let id = commit.id().to_string();
                let short = id.get(..7).unwrap_or(&id);
                return Some(format!(
                    "{short} {}",
                    commit.summary().ok().flatten().unwrap_or_default()
                ));
            }
        }
        None
    }

    /// The dashboard's operator grant: answer the open request for
    /// `digest` when one exists, so it reads answered, or grant directly.
    /// Either way the grant is bound to `digest`, the bytes the operator
    /// reviewed, and records `dashboard` as provenance. The web layer has
    /// already admitted the operator session.
    pub fn grant_script_from_dashboard(
        &self,
        name: &str,
        digest: &str,
        grantor: String,
    ) -> Result<orbit_types::workflow::activity_job::ScriptGrant, OrbitError> {
        let open = self
            .open_script_grant_requests()?
            .into_iter()
            .rev()
            .find(|request| {
                request
                    .script_grant
                    .as_ref()
                    .is_some_and(|grant| grant.name == name && grant.digest == digest)
            });
        if let Some(request) = open {
            self.answer_decision_request_as_human(
                &request.id,
                SCRIPT_GRANT_GRANT,
                None,
                grantor,
                Some("dashboard".to_string()),
            )?;
        } else {
            let catalog = self.v2_script_catalog()?;
            let asset = catalog.get(name).ok_or_else(|| {
                OrbitError::InvalidInput(format!("no script named `{name}` in this workspace"))
            })?;
            let host_role = self
                .verified_script_grant(name)
                .ok()
                .flatten()
                .map_or(ScriptHostRole::Owner, |grant| grant.host_role);
            let requested = ScriptGrantCapabilities::requested_by(&asset.spec, host_role);
            self.grant_script(
                name,
                &ScriptGrantOptions {
                    network: Some(requested.network),
                    follower: host_role == ScriptHostRole::Follower,
                    timeout_seconds: requested.timeout_seconds,
                    output_max_files: requested.output_max_files,
                    output_max_file_bytes: requested.output_max_file_bytes,
                    grantor: Some(grantor),
                    provenance: Some("dashboard".to_string()),
                    expected_digest: Some(digest.to_string()),
                    ..Default::default()
                },
            )?;
        }
        self.verified_script_grant(name)?
            .ok_or_else(|| OrbitError::Execution(format!("script `{name}` grant was not recorded")))
    }
}

fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}
