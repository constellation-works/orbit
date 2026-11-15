//! Bounded, provider-classified invocation evidence shared by health and routing.
//! A successful pipeline is not evidence that its recovery provider succeeded.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::V2AuditEventFilter;
use orbit_types::workflow::{JobRun, ProviderFailureClass, V2AuditEvent, V2AuditEventKind};

use super::auth_failures::{SCAN_LIMIT, host_of};
use crate::OrbitRuntime;
use crate::application::contracts::canonical_provider;

pub(super) struct ProviderInvocationObservation {
    pub run_id: String,
    pub host: String,
    pub provider: String,
    pub crew: Option<String>,
    pub account: Option<String>,
    pub started_at: DateTime<Utc>,
    pub at: DateTime<Utc>,
    pub success: bool,
    pub class: Option<ProviderFailureClass>,
}

impl OrbitRuntime {
    /// Read completed invocations independently of their parent run's status.
    /// This includes secondary recovery on failed, held, recovered and live runs.
    /// Legacy invocations lack trusted outcomes and are deliberately unknown.
    pub(super) fn provider_invocation_observations(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderInvocationObservation>, OrbitError> {
        let rows = self.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: self.workspace_id()?,
            since: Some(since),
            source: Some("v2_envelope".into()),
            event_type: Some("provider.invocation.observed".into()),
            limit: Some(SCAN_LIMIT),
            body_kinds: vec!["provider_invocation_observed".into()],
            ..Default::default()
        })?;
        let fallback = self.fallback_host();
        let mut runs: HashMap<String, Option<JobRun>> = HashMap::new();
        let mut observations = Vec::new();
        for row in rows {
            let Ok(event) = serde_json::from_str::<V2AuditEvent>(&row.payload_json) else {
                continue;
            };
            let V2AuditEventKind::ProviderInvocationObserved {
                provider,
                crew,
                account,
                started_at,
                finished_at,
                success,
                failure_class,
            } = event.kind
            else {
                continue;
            };
            if finished_at < since || started_at > finished_at {
                continue;
            }
            if !runs.contains_key(&row.run_id) {
                runs.insert(
                    row.run_id.clone(),
                    self.stores().jobs().get_job_run(&row.run_id)?,
                );
            }
            let Some(run) = runs.get(&row.run_id).and_then(Option::as_ref) else {
                continue;
            };
            observations.push(ProviderInvocationObservation {
                run_id: row.run_id,
                host: host_of(run, &fallback),
                provider: canonical_provider(&provider),
                crew,
                account,
                started_at,
                at: finished_at,
                success,
                class: failure_class,
            });
        }
        Ok(observations)
    }
}
