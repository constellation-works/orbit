//! Host-only category enablements for precedent-answered decisions.
//!
//! An enablement is written only by the operator's `orbit decision assist
//! enable`, from a `precedent-decision-readout/1` that met every registered
//! target under the exact live host grant. It lives beside the decision
//! channels under `<global>/state/decision-channels/`, which agent and plugin
//! sandboxes mask, and never in workspace configuration. A grant or model
//! revision change suspends it; the rolling window demotes it; only the
//! operator enables it again.
//!
//! Design: `docs/design/decision-assistance/specs/precedent-decisions.md`
//! (Measured gate and enablement, Drift).
use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockOptions, ReportScopedFileLockGuard, acquire_exclusive_file_lock_with_report_scope,
    create_private_dir_all, write_new_private_text,
};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{DecisionGate, MODEL_ANSWER_ACTOR, PRECEDENT_ACTOR};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::contract::PRECEDENT_DECISION_USE_CASE;
use crate::OrbitRuntime;
use crate::application::assistance::AssistanceServicePort;
use crate::application::assistance::{AssistancePolicyIdentity, AssistanceService};
use crate::runtime::plugin::paths::decision_channel_store_dir;
use crate::runtime::plugin::sandbox_mask::{decision_channel_store_masked, not_visible};

const STORE_FILE: &str = "precedent-assist.json";
const LOCK_FILE: &str = "precedent-assist.lock";
const STORE_SCHEMA_VERSION: u32 = 1;
const READOUT_FORMAT: &str = "decision-assistance-eval/4";
const READOUT_VERSION: &str = "precedent-decision-readout/1";

/// Seconds the channel waits for an enabled category's assessment before
/// notifying the human.
pub const PRECEDENT_ASSIST_WAIT_SECONDS: u64 = 120;
/// Share of an enabled category's requests that go to the human anyway.
pub const PRECEDENT_ASSIST_HOLDBACK_SHARE: f64 = 0.10;
/// Scored live outcomes the rolling demotion window keeps.
pub const PRECEDENT_ASSIST_DEMOTION_WINDOW: u32 = 30;
/// Vetoes in the window that demote the category.
pub const PRECEDENT_ASSIST_MAX_VETOES: u32 = 2;
/// Age past which a precedent no longer counts as a recent contradiction.
pub const PRECEDENT_HALF_LIFE_DAYS: i64 = 90;

/// Where an enablement stands. Only `enabled` lets the model answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecedentAssistState {
    /// The operator enabled the category under the live grant.
    Enabled,
    /// The operator disabled it.
    Disabled,
    /// The rolling window fell below its bar or reached `max_vetoes`.
    Demoted,
    /// The live grant's digest or model revision changed.
    Suspended,
}

impl PrecedentAssistState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Demoted => "demoted",
            Self::Suspended => "suspended",
        }
    }
}

/// Where a scored live outcome came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecedentAssistOutcomeSource {
    /// A holdback prediction scored against the human's answer.
    Holdback,
    /// An excluded-tier prediction scored against the human's answer.
    ExcludedTier,
    /// A model answer a human confirmed.
    Confirmed,
    /// A model answer a human vetoed.
    Vetoed,
}

/// One scored live outcome in the rolling window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistOutcome {
    pub request_id: String,
    pub at: DateTime<Utc>,
    pub source: PrecedentAssistOutcomeSource,
    pub agreed: bool,
}

/// What the rolling window held when it demoted the category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistDemotion {
    /// `accuracy` (Wilson lower bound below `min_accuracy`) or `max_vetoes`.
    pub trigger: String,
    pub outcomes: u32,
    pub agreed: u32,
    pub vetoes: u32,
    pub wilson_lower: Option<f64>,
    pub min_accuracy: f64,
    pub max_vetoes: u32,
    pub demotion_window: u32,
    /// The window's request ids, oldest first.
    pub request_ids: Vec<String>,
}

/// Why an enablement stopped answering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistEnd {
    pub at: DateTime<Utc>,
    /// The operator who disabled it, or `system`.
    pub by: String,
    /// `disabled`, `demoted`, `grant_changed`, `model_revision_changed` or
    /// `grant_withdrawn`.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demotion: Option<PrecedentAssistDemotion>,
}

/// One category's enablement record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistEnablement {
    pub category: String,
    pub state: PrecedentAssistState,
    pub policy_version: String,
    pub policy_digest: String,
    pub model_revision: String,
    pub threshold: f64,
    pub min_accuracy: f64,
    /// The readout it was enabled from, as `<task>/<artifact>`.
    pub readout: String,
    pub operator: String,
    pub enabled_at: DateTime<Utc>,
    pub assist_wait_seconds: u64,
    pub holdback_share: f64,
    pub demotion_window: u32,
    pub max_vetoes: u32,
    #[serde(default)]
    pub window: Vec<PrecedentAssistOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<PrecedentAssistEnd>,
}

// Thresholds and bars come from a checked readout and the window's Wilson
// bound is finite, so no stored record holds a NaN and equality is total.
impl Eq for PrecedentAssistEnablement {}

impl PrecedentAssistEnablement {
    /// The workspace the category belongs to: everything before its first `/`.
    pub(super) fn workspace(&self) -> &str {
        self.category.split('/').next().unwrap_or_default()
    }

    /// Whether this record may answer under `live`, the workspace's live
    /// shadow grant. A stored `enabled` whose grant moved reads suspended.
    pub(super) fn answers_under(&self, live: Option<&AssistancePolicyIdentity>) -> bool {
        self.state == PrecedentAssistState::Enabled && self.suspension(live).is_none()
    }

    /// The suspension reason `live` implies for an `enabled` record.
    pub(super) fn suspension(
        &self,
        live: Option<&AssistancePolicyIdentity>,
    ) -> Option<&'static str> {
        match live {
            None => Some("grant_withdrawn"),
            Some(live) if live.model_revision != self.model_revision => {
                Some("model_revision_changed")
            }
            Some(live) if live.digest != self.policy_digest => Some("grant_changed"),
            Some(_) => None,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrecedentAssistStore {
    schema_version: u32,
    /// Host secret keying the holdback hash, so no raiser can pick it.
    holdback_key: String,
    #[serde(default)]
    pub(super) categories: BTreeMap<String, PrecedentAssistEnablement>,
}

impl PrecedentAssistStore {
    /// Whether `request_id` in `category` is in the holdback share.
    pub(super) fn held_back(&self, category: &str, request_id: &str, share: f64) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(self.holdback_key.as_bytes());
        hasher.update([0]);
        hasher.update(category.as_bytes());
        hasher.update([0]);
        hasher.update(request_id.as_bytes());
        let digest = hasher.finalize();
        let mut head = [0u8; 8];
        head.copy_from_slice(&digest[..8]);
        let draw = u64::from_be_bytes(head) as f64 / u64::MAX as f64;
        draw < share
    }
}

/// One category as `orbit decision assist status` shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrecedentAssistStatus {
    /// The state that governs answering now: a stored `enabled` whose live
    /// grant changed reads `suspended`.
    pub effective_state: String,
    #[serde(flatten)]
    pub enablement: PrecedentAssistEnablement,
}

/// Wilson 95% lower bound of `successes` out of `total`.
pub(super) fn wilson_lower(successes: u32, total: u32) -> Option<f64> {
    if total == 0 {
        return None;
    }
    let z = 1.959_963_984_540_054_f64;
    let n = f64::from(total);
    let p = f64::from(successes) / n;
    let denominator = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denominator;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denominator;
    Some((center - half).max(0.0))
}

/// The window's demotion, if it now demotes the category. Vetoes demote at
/// once; the accuracy bar is judged once the window holds
/// `demotion_window` outcomes.
pub(super) fn window_demotion(
    record: &PrecedentAssistEnablement,
) -> Option<PrecedentAssistDemotion> {
    let outcomes = u32::try_from(record.window.len()).unwrap_or(u32::MAX);
    let agreed = u32::try_from(record.window.iter().filter(|o| o.agreed).count()).unwrap_or(0);
    let vetoes = u32::try_from(
        record
            .window
            .iter()
            .filter(|o| o.source == PrecedentAssistOutcomeSource::Vetoed)
            .count(),
    )
    .unwrap_or(0);
    let lower = wilson_lower(agreed, outcomes);
    let trigger = if vetoes >= record.max_vetoes {
        "max_vetoes"
    } else if outcomes >= record.demotion_window && lower.is_some_and(|l| l < record.min_accuracy) {
        "accuracy"
    } else {
        return None;
    };
    Some(PrecedentAssistDemotion {
        trigger: trigger.into(),
        outcomes,
        agreed,
        vetoes,
        wilson_lower: lower,
        min_accuracy: record.min_accuracy,
        max_vetoes: record.max_vetoes,
        demotion_window: record.demotion_window,
        request_ids: record.window.iter().map(|o| o.request_id.clone()).collect(),
    })
}

fn refuse_outside_operator(verb: &str) -> Result<(), OrbitError> {
    if orbit_common::governance::authorization::agent_context_declared() {
        return Err(OrbitError::PolicyDenied(format!(
            "{verb} a precedent-decision category is the operator's: it is refused from an agent envelope"
        )));
    }
    Ok(())
}

fn operator_label(operator: &str) -> Result<String, OrbitError> {
    let operator = operator.trim();
    if operator.is_empty()
        || operator.chars().any(char::is_control)
        || is_automation_actor(operator)
        || operator == MODEL_ANSWER_ACTOR
        || operator == PRECEDENT_ACTOR
    {
        return Err(OrbitError::PolicyDenied(format!(
            "`{operator}` is not a human operator; only the operator enables or disables a precedent-decision category"
        )));
    }
    Ok(operator.to_string())
}

impl OrbitRuntime {
    fn precedent_assist_dir(&self) -> PathBuf {
        decision_channel_store_dir(&self.global_root())
    }

    fn refuse_masked_precedent_assist_store(&self) -> Result<(), OrbitError> {
        if decision_channel_store_masked(&self.global_root()) {
            return Err(not_visible("the precedent-decision enablement store"));
        }
        Ok(())
    }

    pub(super) fn precedent_assist_store(&self) -> Result<PrecedentAssistStore, OrbitError> {
        self.refuse_masked_precedent_assist_store()?;
        let path = self.precedent_assist_dir().join(STORE_FILE);
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PrecedentAssistStore::default());
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(not_visible("the precedent-decision enablement store"));
            }
            Err(error) => {
                return Err(OrbitError::Io(format!("read {}: {error}", path.display())));
            }
        };
        let store: PrecedentAssistStore = serde_json::from_str(&body).map_err(|error| {
            OrbitError::Execution(format!(
                "precedent-decision enablement store at {} is unreadable: {error}",
                path.display()
            ))
        })?;
        if store.schema_version != STORE_SCHEMA_VERSION {
            return Err(OrbitError::Execution(format!(
                "precedent-decision enablement store at {} has unsupported schema {}",
                path.display(),
                store.schema_version
            )));
        }
        Ok(store)
    }

    pub(super) fn lock_precedent_assist_store(
        &self,
    ) -> Result<ReportScopedFileLockGuard, OrbitError> {
        self.refuse_masked_precedent_assist_store()?;
        let dir = self.precedent_assist_dir();
        create_private_dir_all(&dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
        let lock_path = dir.join(LOCK_FILE);
        acquire_exclusive_file_lock_with_report_scope(
            &lock_path,
            "precedent-decision enablements",
            FileLockOptions::default(),
        )
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::PermissionDenied => {
                not_visible("the precedent-decision enablement store")
            }
            _ => OrbitError::Io(format!("lock {}: {error}", lock_path.display())),
        })
    }

    /// Callers hold [`Self::lock_precedent_assist_store`] across their
    /// read-modify-write.
    pub(super) fn write_precedent_assist_store(
        &self,
        store: &mut PrecedentAssistStore,
    ) -> Result<(), OrbitError> {
        store.schema_version = STORE_SCHEMA_VERSION;
        if store.holdback_key.is_empty() {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|error| {
                OrbitError::Execution(format!("draw precedent holdback key: {error}"))
            })?;
            store.holdback_key = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        }
        let dir = self.precedent_assist_dir();
        create_private_dir_all(&dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
        let path = dir.join(STORE_FILE);
        let body = serde_json::to_string(store).map_err(|error| {
            OrbitError::Execution(format!("serialize precedent-decision enablements: {error}"))
        })?;
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce)
            .map_err(|error| OrbitError::Execution(format!("draw staging name: {error}")))?;
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let staged = dir.join(format!(".{STORE_FILE}.{nonce}.tmp"));
        let result =
            write_new_private_text(&staged, &body).and_then(|()| std::fs::rename(&staged, &path));
        if let Err(error) = result {
            let _ = std::fs::remove_file(&staged);
            return Err(OrbitError::Io(format!("write {}: {error}", path.display())));
        }
        Ok(())
    }

    /// Enable `category` from the readout artifact `readout` (`<task>/<path>`)
    /// as `operator`. Refused from an agent envelope, for a category outside
    /// this workspace or on a gate the model never answers, without a live
    /// shadow grant, and for a readout that is not `/4`, whose policy digest
    /// differs from the live grant, whose labels are synthetic, or whose
    /// category missed a registered target.
    pub fn enable_precedent_assist(
        &self,
        category: &str,
        readout: &str,
        operator: &str,
    ) -> Result<PrecedentAssistStatus, OrbitError> {
        let service = self.assistance_service()?;
        self.enable_precedent_assist_with_service(category, readout, operator, &service)
    }

    /// [`Self::enable_precedent_assist`] against the caller's assistance service.
    pub fn enable_precedent_assist_with_service(
        &self,
        category: &str,
        readout: &str,
        operator: &str,
        service: &AssistanceService,
    ) -> Result<PrecedentAssistStatus, OrbitError> {
        refuse_outside_operator("enabling")?;
        let operator = operator_label(operator)?;
        let workspace = self.workspace_id()?;
        let category = category.trim();
        let kind = category
            .strip_prefix(&format!("{workspace}/"))
            .filter(|kind| !kind.is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "category `{category}` is not in this workspace; a category is `{workspace}/<gate>` or `{workspace}/agent_request/<topic file>`"
                ))
            })?;
        let gate = kind.split('/').next().unwrap_or_default();
        if matches!(
            DecisionGate::from_name(gate),
            Some(
                DecisionGate::ModelAnswerReview
                    | DecisionGate::ResidentAuthority
                    | DecisionGate::ScriptGrant
            ) | None
        ) {
            return Err(OrbitError::InvalidInput(format!(
                "category `{category}` names no gate the model may answer"
            )));
        }
        let live = service
            .assistance_shadow_policy_identity(&workspace, PRECEDENT_DECISION_USE_CASE)?
            .ok_or_else(|| {
                OrbitError::PolicyDenied(
                    "this workspace has no live shadow `precedent_decision` grant; a category is enabled only under the grant its readout measured".into(),
                )
            })?;
        let (task_id, path) = readout.trim().split_once('/').ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "readout `{readout}` must be `<task>/<artifact path>`"
            ))
        })?;
        let artifact = self
            .get_task_artifact_inline(task_id, path)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("readout artifact `{readout}` does not exist"))
            })?;
        let document: Value = serde_json::from_slice(&artifact.content).map_err(|_| {
            OrbitError::InvalidInput(format!("readout `{readout}` is not a JSON document"))
        })?;
        let targets = check_readout(&document, category, &live.digest).map_err(|why| {
            OrbitError::PolicyDenied(format!("readout `{readout}` {why}; nothing was enabled"))
        })?;
        let _lock = self.lock_precedent_assist_store()?;
        let mut store = self.precedent_assist_store()?;
        let record = PrecedentAssistEnablement {
            category: category.to_string(),
            state: PrecedentAssistState::Enabled,
            policy_version: live.policy_version.clone(),
            policy_digest: live.digest.clone(),
            model_revision: live.model_revision.clone(),
            threshold: targets.threshold,
            min_accuracy: targets.min_accuracy,
            readout: format!("{task_id}/{path}"),
            operator,
            enabled_at: Utc::now(),
            assist_wait_seconds: PRECEDENT_ASSIST_WAIT_SECONDS,
            holdback_share: PRECEDENT_ASSIST_HOLDBACK_SHARE,
            demotion_window: PRECEDENT_ASSIST_DEMOTION_WINDOW,
            max_vetoes: PRECEDENT_ASSIST_MAX_VETOES,
            window: Vec::new(),
            ended: None,
        };
        store
            .categories
            .insert(category.to_string(), record.clone());
        self.write_precedent_assist_store(&mut store)?;
        Ok(PrecedentAssistStatus {
            effective_state: PrecedentAssistState::Enabled.as_str().into(),
            enablement: record,
        })
    }

    /// Disable `category` immediately: no further model answer is applied in
    /// it, and its open requests wait for the human. Refused from an agent
    /// envelope.
    pub fn disable_precedent_assist(
        &self,
        category: &str,
        operator: &str,
    ) -> Result<PrecedentAssistStatus, OrbitError> {
        refuse_outside_operator("disabling")?;
        let operator = operator_label(operator)?;
        let category = category.trim();
        let _lock = self.lock_precedent_assist_store()?;
        let mut store = self.precedent_assist_store()?;
        let record = store.categories.get_mut(category).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "category `{category}` has no enablement on this host"
            ))
        })?;
        if record.state == PrecedentAssistState::Enabled {
            record.state = PrecedentAssistState::Disabled;
            record.ended = Some(PrecedentAssistEnd {
                at: Utc::now(),
                by: operator,
                reason: "disabled".into(),
                demotion: None,
            });
        }
        let record = record.clone();
        self.write_precedent_assist_store(&mut store)?;
        Ok(PrecedentAssistStatus {
            effective_state: record.state.as_str().into(),
            enablement: record,
        })
    }

    /// Every category enablement on this host, with the state that governs
    /// answering now.
    pub fn precedent_assist_status(&self) -> Result<Vec<PrecedentAssistStatus>, OrbitError> {
        let service = self.assistance_service()?;
        self.precedent_assist_status_with_service(&service)
    }

    /// [`Self::precedent_assist_status`] against the caller's assistance service.
    pub fn precedent_assist_status_with_service(
        &self,
        service: &AssistanceService,
    ) -> Result<Vec<PrecedentAssistStatus>, OrbitError> {
        let store = self.precedent_assist_store()?;
        let mut live = BTreeMap::new();
        let mut out = Vec::new();
        for record in store.categories.values() {
            let workspace = record.workspace().to_string();
            let identity = match live.get(&workspace) {
                Some(identity) => identity,
                None => {
                    let identity = service
                        .assistance_shadow_policy_identity(&workspace, PRECEDENT_DECISION_USE_CASE)
                        .unwrap_or(None);
                    live.entry(workspace).or_insert(identity)
                }
            };
            let effective = if record.state == PrecedentAssistState::Enabled
                && record.suspension(identity.as_ref()).is_some()
            {
                PrecedentAssistState::Suspended
            } else {
                record.state
            };
            out.push(PrecedentAssistStatus {
                effective_state: effective.as_str().into(),
                enablement: record.clone(),
            });
        }
        Ok(out)
    }

    /// This workspace's demoted categories, for the decision digest. Empty
    /// when the store is unreadable from this process.
    pub(crate) fn precedent_assist_demotions(&self) -> Vec<PrecedentAssistEnablement> {
        let Ok(workspace) = self.workspace_id() else {
            return Vec::new();
        };
        self.precedent_assist_store()
            .map(|store| {
                store
                    .categories
                    .into_values()
                    .filter(|record| {
                        record.state == PrecedentAssistState::Demoted
                            && record.workspace() == workspace
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

struct ReadoutTargets {
    threshold: f64,
    min_accuracy: f64,
}

/// Check one readout category against its registered targets and the live
/// grant's digest. The readout reports metrics; this decides they were met.
fn check_readout(
    document: &Value,
    category: &str,
    live_digest: &str,
) -> Result<ReadoutTargets, String> {
    if document["format_version"] != READOUT_FORMAT
        || document["readout_version"] != READOUT_VERSION
    {
        return Err(format!("is not a `{READOUT_VERSION}` readout"));
    }
    if document["policy_digest"].as_str() != Some(live_digest) {
        return Err(format!(
            "was measured under policy digest `{}`, not the live grant's `{live_digest}`",
            document["policy_digest"].as_str().unwrap_or("none")
        ));
    }
    if document["validation_errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err("carries validation errors".into());
    }
    if document["evidence_status"]
        .as_str()
        .is_none_or(|status| !status.starts_with("recorded human decisions"))
    {
        return Err("does not score recorded human decisions only".into());
    }
    let row = &document["categories"][category];
    if !row.is_object() {
        return Err(format!("has no category `{category}`"));
    }
    let targets = &row["targets"];
    let number = |value: &Value, name: &str| {
        value
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("has no numeric `{name}` for `{category}`"))
    };
    let threshold = number(&targets["threshold"], "threshold")?;
    let min_samples = number(&targets["min_samples"], "min_samples")?;
    let min_accuracy = number(&targets["min_accuracy"], "min_accuracy")?;
    let min_coverage = number(&targets["min_coverage"], "min_coverage")?;
    let max_ece = number(&targets["max_ece"], "max_ece")?;
    if !(threshold > 0.0 && threshold <= 1.0) || !(0.0..=1.0).contains(&min_accuracy) {
        return Err(format!("registers out-of-range targets for `{category}`"));
    }
    let scored = number(&row["scored"], "scored")?;
    let lower = number(&row["accuracy"]["interval_95"][0], "accuracy lower bound")?;
    let coverage = number(&row["coverage"]["value"], "coverage")?;
    let ece = number(&row["ece"]["value"], "ece")?;
    if (row["threshold"].as_f64()).is_some_and(|t| (t - threshold).abs() > f64::EPSILON) {
        return Err(format!(
            "disagrees with itself on the `{category}` threshold"
        ));
    }
    let mut missed = Vec::new();
    if scored < min_samples {
        missed.push(format!("scored {scored} < min_samples {min_samples}"));
    }
    if lower < min_accuracy {
        missed.push(format!(
            "accuracy lower bound {lower:.3} < min_accuracy {min_accuracy}"
        ));
    }
    if coverage < min_coverage {
        missed.push(format!(
            "coverage {coverage:.3} < min_coverage {min_coverage}"
        ));
    }
    if ece > max_ece {
        missed.push(format!("ece {ece:.3} > max_ece {max_ece}"));
    }
    if !missed.is_empty() {
        return Err(format!(
            "missed registered targets for `{category}`: {}",
            missed.join("; ")
        ));
    }
    Ok(ReadoutTargets {
        threshold,
        min_accuracy,
    })
}

/// Test-support tuning so a regression check is deterministic: the holdback
/// share and window size are otherwise the fixed defaults above.
#[cfg(feature = "test-support")]
impl OrbitRuntime {
    /// Set `category`'s holdback share and demotion window. No production
    /// surface calls this.
    pub fn tune_precedent_assist_for_test(
        &self,
        category: &str,
        holdback_share: f64,
        demotion_window: u32,
    ) -> Result<(), OrbitError> {
        let _lock = self.lock_precedent_assist_store()?;
        let mut store = self.precedent_assist_store()?;
        let record = store
            .categories
            .get_mut(category)
            .ok_or_else(|| OrbitError::InvalidInput(format!("no enablement for `{category}`")))?;
        record.holdback_share = holdback_share;
        record.demotion_window = demotion_window;
        self.write_precedent_assist_store(&mut store)
    }
}
