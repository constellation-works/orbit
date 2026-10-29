//! Optional assistance application port. Consumers supply immutable, selected
//! evidence; this service cannot mutate tasks, reviews, human answers or jobs.

mod preparation;
mod templates;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_store::contracts::assistance::{
    AssistanceAdmission, AssistanceClaim, AssistanceRecord, AssistanceStoreBackend,
};
use orbit_types::assistance::{
    AssistanceDisposition, AssistanceMode, AssistanceReason, AssistanceRequest, AssistanceResult,
};

pub(crate) use preparation::prepare_assistance_evidence;

/// Inject a recorded adapter at the same public boundary used by consumers.
/// Implementations must honor the claim's timeout and bounded response size,
/// never log request/response/credential values, and return fixed error codes.
pub trait AssistanceAdapter: Send + Sync {
    /// Resolve credentials only within the adapter and redact their values
    /// along with supplied secrets before any persistence or transmission.
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        supplied_secrets: &[String],
        policy: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason>;
    /// A refusal decided before outbound I/O must be one of the reasons that
    /// `refusal_precedes_outbound_io` lists; any other result is an attempted request.
    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        policy: &AssistancePolicy,
    ) -> AssistanceResult;
}

/// Adapter refusals decided before the transport is entered. Their claims release
/// the reservation and record no outcome-unknown attempt.
fn refusal_precedes_outbound_io(reason: AssistanceReason) -> bool {
    matches!(
        reason,
        AssistanceReason::CredentialUnavailable
            | AssistanceReason::ConfigurationChanged
            | AssistanceReason::InvalidRequest
            | AssistanceReason::DeadlineBeforeSend
    )
}

/// Reloaded before admission, before sending, after sending and before reads.
/// The composition loader reads only the operator's global configuration.
pub type AssistanceSettingsLoader =
    Arc<dyn Fn() -> Result<AssistanceSettings, OrbitError> + Send + Sync>;
pub type AssistanceClock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct AssistanceService {
    store: Arc<dyn AssistanceStoreBackend>,
    adapter: Arc<dyn AssistanceAdapter>,
    settings: AssistanceSettingsLoader,
    clock: AssistanceClock,
}

impl AssistanceService {
    /// Local host-grant identity. This neither resolves credentials nor writes
    /// work; consumers use it to leave disabled workspaces entirely untouched.
    pub fn assistance_policy_epoch(
        &self,
        workspace: &str,
        use_case: &str,
    ) -> Result<Option<String>, OrbitError> {
        let settings = (self.settings)()?;
        settings.validate_assistance_settings()?;
        settings
            .assistance_policy(workspace, use_case)
            .filter(|p| p.mode != AssistanceMode::Off)
            .map(|p| preparation::assistance_policy_digest(&settings, p))
            .transpose()
    }

    /// Does not start workers, resolve credentials or activate consumers.
    pub fn new(
        store: Arc<dyn AssistanceStoreBackend>,
        adapter: Arc<dyn AssistanceAdapter>,
        settings: AssistanceSettingsLoader,
    ) -> Self {
        Self {
            store,
            adapter,
            settings,
            clock: Arc::new(Utc::now),
        }
    }

    /// Deterministic fixture clock; production composition uses UTC wall time.
    pub fn with_assistance_clock(mut self, clock: AssistanceClock) -> Self {
        self.clock = clock;
        self
    }

    /// Default off returns before redaction, queue writes or credential reads.
    pub fn enqueue_assistance(
        &self,
        request: AssistanceRequest,
        supplied_secrets: &[String],
    ) -> Result<AssistanceAdmission, OrbitError> {
        self.enqueue_assistance_template(request, supplied_secrets, None)
    }

    /// Admit bounded expansions of a host-granted question template. Expanded
    /// names and criterion references cannot change its questions or rubric.
    pub fn enqueue_expanded_assistance(
        &self,
        request: AssistanceRequest,
        supplied_secrets: &[String],
        template: &orbit_types::assistance::AssistanceContract,
    ) -> Result<AssistanceAdmission, OrbitError> {
        self.enqueue_assistance_template(request, supplied_secrets, Some(template))
    }

    fn enqueue_assistance_template(
        &self,
        request: AssistanceRequest,
        supplied_secrets: &[String],
        template: Option<&orbit_types::assistance::AssistanceContract>,
    ) -> Result<AssistanceAdmission, OrbitError> {
        let settings = (self.settings)()?;
        settings.validate_assistance_settings()?;
        let Some(policy) = settings
            .assistance_policy(&request.workspace_id, &request.use_case)
            .filter(|p| p.mode != AssistanceMode::Off)
        else {
            return Ok(AssistanceAdmission::Disabled);
        };
        let now = (self.clock)();
        let work = preparation::prepare_assistance_work(
            request,
            supplied_secrets,
            policy,
            &settings,
            self.adapter.as_ref(),
            now,
            template,
        )
        .map_err(|_| {
            OrbitError::InvalidInput(
                "assistance request exceeds its granted contract, evidence scope or bounds".into(),
            )
        })?;
        self.store.assistance_enqueue(&work)
    }

    /// One bounded unit of owner work. Scheduling and consumers are opt-in.
    /// The store transaction ends before the adapter is entered.
    pub fn process_next_assistance(
        &self,
        workspace: &str,
        use_case: &str,
    ) -> Result<Option<String>, OrbitError> {
        self.process_next_assistance_checked(workspace, use_case, |_| Ok(true))
    }

    /// Consumer freshness fence, checked outside store transactions immediately
    /// before and after I/O. A superseded subject is retained as a stale attempt.
    pub fn process_next_assistance_checked(
        &self,
        workspace: &str,
        use_case: &str,
        is_current: impl Fn(&AssistanceRequest) -> Result<bool, OrbitError>,
    ) -> Result<Option<String>, OrbitError> {
        let settings = (self.settings)()?;
        settings.validate_assistance_settings()?;
        let Some(policy) = settings
            .assistance_policy(workspace, use_case)
            .filter(|p| p.mode != AssistanceMode::Off)
        else {
            return Ok(None);
        };
        let digest = preparation::assistance_policy_digest(&settings, policy)?;
        let Some(claim) =
            self.store
                .assistance_claim(workspace, use_case, &digest, (self.clock)())?
        else {
            return Ok(None);
        };
        // A claim may have waited for a store lock. Recheck off/configuration
        // and deadline at the final local boundary before outbound I/O.
        let before = (self.settings)()?;
        let before_matches = before
            .assistance_policy(workspace, use_case)
            .is_some_and(|p| {
                p.mode != AssistanceMode::Off
                    && preparation::assistance_policy_digest(&before, p)
                        .ok()
                        .as_ref()
                        == Some(&digest)
            });
        // Only a claim whose request entered outbound I/O keeps its reservation.
        let mut attempted = false;
        let mut result = if !is_current(&claim.work.request)? {
            AssistanceResult::assistance_gap(AssistanceReason::Superseded)
        } else if !before_matches {
            AssistanceResult::assistance_gap(AssistanceReason::ConfigurationChanged)
        } else if (self.clock)() >= claim.deadline {
            AssistanceResult::assistance_gap(AssistanceReason::DeadlineBeforeSend)
        } else {
            let evaluated = self.adapter.evaluate_assistance(&claim, policy);
            attempted = !refusal_precedes_outbound_io(evaluated.reason);
            evaluated
        };
        let after = (self.settings)();
        let after_matches = after
            .as_ref()
            .ok()
            .and_then(|s| s.assistance_policy(workspace, use_case).map(|p| (s, p)))
            .is_some_and(|(s, p)| {
                p.mode != AssistanceMode::Off
                    && preparation::assistance_policy_digest(s, p).ok().as_ref() == Some(&digest)
            });
        if !after_matches {
            result = AssistanceResult::assistance_gap(AssistanceReason::ConfigurationChanged);
        } else if !is_current(&claim.work.request)? {
            result = AssistanceResult::assistance_gap(AssistanceReason::Superseded);
        }
        result.validate_result(&claim.work.request.contract);
        // Returned vendor strings are never an arbitrary persisted text channel.
        if result
            .returned_model
            .as_ref()
            .is_some_and(|m| m != &claim.work.requested_model && m != &claim.work.model_revision)
        {
            result = AssistanceResult::assistance_gap(AssistanceReason::Malformed);
        }
        result.pricing_version = if result.usage.is_some() {
            claim.work.budget.pricing_version.clone()
        } else {
            None
        };
        result.estimated_cost_microusd = result
            .usage
            .as_ref()
            .and_then(|u| claim.work.budget.assistance_charge_bound(u.input_tokens));
        if attempted {
            self.store
                .assistance_finish(&claim, result, (self.clock)())?;
        } else {
            self.store
                .assistance_finish_unattempted(&claim, result, (self.clock)())?;
        }
        Ok(Some(claim.work.id))
    }

    /// Operator/history view, including pending, shadow and explicit gaps.
    /// The consumer supplies its current semantic digest on every read.
    pub fn assistance_assessment(
        &self,
        id: &str,
        source_digest: &str,
    ) -> Result<Option<AssistanceRecord>, OrbitError> {
        let Some(mut record) = self.store.assistance_record(id)? else {
            return Ok(None);
        };
        let settings = (self.settings)()?;
        let policy = settings.assistance_policy(
            &record.work.request.workspace_id,
            &record.work.request.use_case,
        );
        // Queue age bounds queued work only. A completed result stays
        // current while its policy and source digest still match, until
        // retention maintenance removes it (at created_at + queue age +
        // retention) or a fresher snapshot moves its slot.
        let identity_matches = policy.is_some_and(|p| {
            p.mode != AssistanceMode::Off
                && preparation::assistance_policy_digest(&settings, p)
                    .ok()
                    .as_ref()
                    == Some(&record.work.policy_digest)
        }) && record.work.request.source_digest == source_digest;
        let completed = record.result.is_some();
        let queue_fresh = completed
            || (self.clock)()
                < record.work.created_at
                    + chrono::Duration::seconds(record.work.limits.queue_age_seconds.into());
        // Only a moved slot or a changed policy/source identity is real
        // supersession; queue-age expiry of queued work is not.
        let superseded = !record.current || !identity_matches;
        if superseded || !queue_fresh {
            record.current = false;
            if superseded && let Some(result) = &mut record.result {
                result.disposition = AssistanceDisposition::Stale;
                result.reason = AssistanceReason::Superseded;
                result.answers.clear();
            }
        }
        Ok(Some(record))
    }

    /// Advisory view is absent in off/shadow modes and for stale snapshots.
    /// Suggested/uncertain answers remain annotations, never permissions.
    pub fn assistance_advisory(
        &self,
        id: &str,
        source_digest: &str,
    ) -> Result<Option<AssistanceResult>, OrbitError> {
        Ok(self
            .assistance_assessment(id, source_digest)?
            .filter(|r| r.current && r.work.mode == AssistanceMode::Advisory)
            .and_then(|r| r.result))
    }
}
