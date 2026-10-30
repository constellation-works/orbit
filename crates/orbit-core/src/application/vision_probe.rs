//! The image-input probe (`orbit agents vision-probe`) and the visual hold.
//!
//! The probe runs the canary check every image-input invocation receives,
//! outside a delivery: one `image_input_probe_pipeline` run per crew, each
//! staging the canary alone and asking the agent to return its code. A crew
//! is reported as `seen`, `image_not_seen`, or the reason it cannot take
//! images at all (no CLI runtime, no `spec.image_input`, `vision = false`).
//!
//! The visual hold is read from the `image_canary_checked` audit events every
//! canary check emits, probe or delivery: a crew whose newest
//! [`VISUAL_HOLD_CONSECUTIVE_MISSES`] or more checks all missed is held for
//! visual purposes until its last miss plus a backoff that grows like a
//! provider failure hold's. Its next passing check clears it.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::V2AuditEventFilter;
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    ImageInputMode, Provider, V2AuditEvent, V2AuditEventKind, VISUAL_HOLD_CONSECUTIVE_MISSES,
    is_image_not_seen,
};
use serde::Serialize;
use serde_json::json;

use super::executor::declared_image_input;
use super::job::pipeline::PipelineWaitStatus;
use crate::OrbitRuntime;

/// The catalog job one probe run submits.
const PROBE_JOB: &str = "image_input_probe_pipeline";
/// The newest canary checks read per probe or hold lookup.
const CANARY_EVENT_WINDOW: usize = 500;
/// The first visual hold's length; each further miss doubles it.
const VISUAL_HOLD_BASE_MINUTES: i64 = 30;
/// The longest a visual hold lasts after its last miss.
const VISUAL_HOLD_MAX_HOURS: i64 = 24;
/// Longest error detail a probe row carries.
const DETAIL_LIMIT: usize = 240;

/// What one crew's probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisionProbeResult {
    /// The agent returned the canary code.
    Seen,
    /// The agent finished without returning the code.
    ImageNotSeen,
    /// The crew's provider has no CLI runtime.
    NoCliRuntime,
    /// The crew's executor declares no `spec.image_input`.
    NoImageInput,
    /// The crew sets `vision = false`.
    VisionOff,
    /// The run failed before the canary was checked.
    Failed,
    /// The run had not settled when the probe stopped waiting.
    Timeout,
}

impl VisionProbeResult {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::ImageNotSeen => "image_not_seen",
            Self::NoCliRuntime => "no_cli_runtime",
            Self::NoImageInput => "no_image_input",
            Self::VisionOff => "vision_off",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
        }
    }
}

/// One row of the probe table.
#[derive(Debug, Clone, Serialize)]
pub struct VisionProbeRow {
    pub crew: String,
    pub executor: String,
    pub model: String,
    /// The executor's declared mode, when it declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ImageInputMode>,
    pub result: VisionProbeResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The crew's visual hold after this probe, when it is held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<VisualHold>,
}

/// A crew held for visual purposes after consecutive canary misses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisualHold {
    pub consecutive_misses: usize,
    pub last_miss_at: DateTime<Utc>,
    pub until: DateTime<Utc>,
}

/// What `orbit agents vision-probe` reports.
#[derive(Debug, Clone, Serialize)]
pub struct VisionProbeReport {
    pub started_at: DateTime<Utc>,
    pub rows: Vec<VisionProbeRow>,
}

/// Whether a crew can be given images at all, before any probe.
enum VisionCapability {
    Able(ImageInputMode),
    Unable(VisionProbeResult, Option<ImageInputMode>, String),
}

impl OrbitRuntime {
    /// Probe each named crew, or every enabled configured crew when `crews`
    /// is empty. A named crew that cannot take images fails the whole probe
    /// before anything launches, as an explicit image-input selection does
    /// at dispatch; an unnamed one is reported as a row.
    pub fn vision_probe(
        &self,
        crews: &[String],
        timeout_seconds: u64,
    ) -> Result<VisionProbeReport, OrbitError> {
        let started_at = Utc::now();
        let registry = self.context.settings().crews();
        let explicit = !crews.is_empty();
        let selected: Vec<&Crew> = if explicit {
            crews
                .iter()
                .map(|name| {
                    registry
                        .get(name)
                        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown crew `{name}`")))
                })
                .collect::<Result<_, _>>()?
        } else {
            registry.values().filter(|crew| crew.enabled).collect()
        };

        let mut rows = Vec::with_capacity(selected.len());
        let mut launched = BTreeMap::new();
        for crew in selected {
            let executor = crew.assignment.provider.clone();
            let mut row = VisionProbeRow {
                crew: crew.name.clone(),
                executor: executor.clone(),
                model: crew.assignment.model.clone(),
                mode: None,
                result: VisionProbeResult::Failed,
                run_id: None,
                detail: None,
                hold: None,
            };
            match self.vision_capability(crew)? {
                VisionCapability::Unable(result, mode, reason) => {
                    if explicit {
                        return Err(OrbitError::InvalidInput(format!(
                            "image_input_unavailable: executor `{executor}`: {reason}"
                        )));
                    }
                    row.result = result;
                    row.mode = mode;
                    row.detail = Some(reason);
                }
                VisionCapability::Able(mode) => {
                    row.mode = Some(mode);
                    let input = json!({
                        "workspace_path": self.paths().repo_root.display().to_string(),
                        "crew": crew.name,
                        "image_input": { "images": [] },
                    });
                    let invoke = self.submit_pipeline_run(PROBE_JOB, input, None, None)?;
                    row.run_id = Some(invoke.run_id.clone());
                    launched.insert(invoke.run_id, rows.len());
                }
            }
            rows.push(row);
        }

        if !launched.is_empty() {
            let run_ids: Vec<String> = launched.keys().cloned().collect();
            let waited = self.wait_pipeline_runs(&run_ids, timeout_seconds, 2, None)?;
            let checks = self.canary_checks(Some(started_at))?;
            for entry in waited.results {
                let Some(&index) = launched.get(&entry.run_id) else {
                    continue;
                };
                let row = &mut rows[index];
                let seen = checks.get(&row.crew).and_then(|checks| checks.first());
                (row.result, row.detail) = classify(&entry.status, entry.error.as_deref(), seen);
            }
        }
        for row in &mut rows {
            row.hold = self.visual_hold(&row.crew)?;
        }
        Ok(VisionProbeReport { started_at, rows })
    }

    /// The crew's visual hold, when its newest canary checks are
    /// [`VISUAL_HOLD_CONSECUTIVE_MISSES`] or more misses and the backoff
    /// since the last of them has not elapsed.
    pub fn visual_hold(&self, crew: &str) -> Result<Option<VisualHold>, OrbitError> {
        let checks = self.canary_checks(None)?;
        let history = checks.get(crew).map(Vec::as_slice).unwrap_or_default();
        Ok(visual_hold_from(history, Utc::now()))
    }

    fn vision_capability(&self, crew: &Crew) -> Result<VisionCapability, OrbitError> {
        let executor = crew.assignment.provider.as_str();
        if !Provider::parse(executor).is_ok_and(Provider::has_cli_runtime) {
            return Ok(VisionCapability::Unable(
                VisionProbeResult::NoCliRuntime,
                None,
                "it has no CLI runtime, so its crews cannot take images".to_string(),
            ));
        }
        let installed = self.get_executor_def(executor)?;
        let Some(declared) = declared_image_input(installed.as_ref(), executor) else {
            return Ok(VisionCapability::Unable(
                VisionProbeResult::NoImageInput,
                None,
                "it declares no `spec.image_input`, so its crews cannot take images".to_string(),
            ));
        };
        if !crew.vision {
            return Ok(VisionCapability::Unable(
                VisionProbeResult::VisionOff,
                Some(declared.mode),
                format!(
                    "crew `{}` sets `vision = false` in [crews.{}], so it is never given images \
                     despite its executor's `spec.image_input`",
                    crew.name, crew.name
                ),
            ));
        }
        Ok(VisionCapability::Able(declared.mode))
    }

    /// Canary checks per crew since `since`, newest first.
    fn canary_checks(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<BTreeMap<String, Vec<CanaryCheck>>, OrbitError> {
        let rows = self.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: self.workspace_id()?,
            since,
            limit: Some(CANARY_EVENT_WINDOW),
            body_kinds: vec!["image_canary_checked".to_string()],
            ..V2AuditEventFilter::default()
        })?;
        let mut checks: BTreeMap<String, Vec<CanaryCheck>> = BTreeMap::new();
        for row in rows {
            let Ok(event) = serde_json::from_str::<V2AuditEvent>(&row.payload_json) else {
                continue;
            };
            let V2AuditEventKind::ImageCanaryChecked {
                crew: Some(crew),
                seen,
                ..
            } = event.kind
            else {
                continue;
            };
            checks.entry(crew).or_default().push(CanaryCheck {
                at: event.envelope.ts,
                seen,
            });
        }
        for history in checks.values_mut() {
            history.sort_by_key(|check| std::cmp::Reverse(check.at));
        }
        Ok(checks)
    }
}

#[derive(Debug, Clone, Copy)]
struct CanaryCheck {
    at: DateTime<Utc>,
    seen: bool,
}

/// A probe run's result from its wait entry and its newest canary check.
fn classify(
    status: &PipelineWaitStatus,
    error: Option<&str>,
    check: Option<&CanaryCheck>,
) -> (VisionProbeResult, Option<String>) {
    let detail = error.map(bounded);
    if status.is_success() {
        return (VisionProbeResult::Seen, None);
    }
    if check.is_some_and(|check| !check.seen) || is_image_not_seen(None, error) {
        return (VisionProbeResult::ImageNotSeen, detail);
    }
    if status.is_settled() {
        (VisionProbeResult::Failed, detail)
    } else {
        (VisionProbeResult::Timeout, detail)
    }
}

/// The hold `history` (newest first) implies at `now`.
fn visual_hold_from(history: &[CanaryCheck], now: DateTime<Utc>) -> Option<VisualHold> {
    let misses = history.iter().take_while(|check| !check.seen).count();
    if misses < VISUAL_HOLD_CONSECUTIVE_MISSES {
        return None;
    }
    let last_miss_at = history.first()?.at;
    let doublings = u32::try_from(misses - VISUAL_HOLD_CONSECUTIVE_MISSES)
        .unwrap_or(u32::MAX)
        .min(16);
    let backoff = Duration::minutes(VISUAL_HOLD_BASE_MINUTES.saturating_mul(1_i64 << doublings))
        .min(Duration::hours(VISUAL_HOLD_MAX_HOURS));
    let until = last_miss_at + backoff;
    (until > now).then_some(VisualHold {
        consecutive_misses: misses,
        last_miss_at,
        until,
    })
}

fn bounded(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(DETAIL_LIMIT) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}
