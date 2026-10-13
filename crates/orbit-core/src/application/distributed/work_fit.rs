//! Gap fit for a pull drain: what work this host has room for.
//!
//! A follower does not pick its task; the owner does. So instead of fitting
//! one candidate, the drain asks, under the host admission lock, which kinds
//! of work fit under its own load forecast for their profile's whole horizon:
//!
//! - every `no-diff` and `change` complexity, unset included;
//! - every `auto-task:` template this workspace has a profile of on this
//!   host. A template it has never run here is not listed, so under
//!   `enforce` it stays for another executor until this host has run it.
//!
//! Each kind is judged by the local drain's rule, [`ForecastGap::fit`]: while
//! the forecast holds no running delivery or reservation every kind fits, even
//! one whose profile alone exceeds a threshold, so no kind is kept from an
//! idle host for good.
//!
//! Under `machine.auto_concurrency.mode = "enforce"` the request carries the
//! list and the owner admits only a task inside it. When nothing fits, the
//! drain requests nothing. In shadow mode the list is recorded on the drain
//! and the request goes out as with `off`. With `off`, or on a host with no
//! profiles or no readable load, nothing is computed and no list is sent.
//!
//! An enforcing request holds pending capacity in the same transaction as
//! its fit, priced by the maximum at each minute across its admissible profiles.
//! A returned claim atomically replaces that capacity before leaf creation. Each computed fit also
//! records accuracy points and its request decision; a returned claim is
//! added to those points with the same profile lookup as its reservation.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionForecastRecord, AdmissionReceipt, AdmissionRequest, AdmissionWorkFit,
    AdmissionWorkKey, LocalPullAdmission, LocalPullPhase, PullForecastDecision,
};
use orbit_types::task::{
    CHANGE_WORK_CLASS, NO_DIFF_EXPECTED_TAG, NO_DIFF_WORK_CLASS, TaskComplexity, work_class,
};
use orbit_types::workflow::{AUTO_TASK_TAG_PREFIX, PullAdmissionFit, PullUnfitWork};

use crate::OrbitRuntime;
use crate::runtime::host_resource::forecast::{
    AdmissionReservationRequest, ForecastDecision, ForecastGap, ForecastPoints, HostLoadForecast,
    pending_pull_reservation_id,
};
use crate::runtime::host_resource::profile::{BaseClass, ProfileLookup};

const SHADOW: &str = "shadow";
const ENFORCE: &str = "enforce";

/// Every complexity a task can carry, unset first.
const COMPLEXITIES: [Option<TaskComplexity>; 6] = [
    None,
    Some(TaskComplexity::Low),
    Some(TaskComplexity::Medium),
    Some(TaskComplexity::Hard),
    Some(TaskComplexity::XHard),
    Some(TaskComplexity::Unassessed),
];

/// What one pull request may carry, and its record on the drain.
#[derive(Debug, Clone)]
pub(crate) struct PullWorkFit {
    pub(crate) fit: AdmissionWorkFit,
    pub(crate) record: PullAdmissionFit,
    accuracy: ForecastPoints,
    pending: Vec<ProfileLookup>,
}

impl PullWorkFit {
    /// The list the request carries: under `enforce` only.
    pub(crate) fn admissible(&self) -> Option<AdmissionWorkFit> {
        self.record.sent.then(|| self.fit.clone())
    }

    /// Whether a request may go out: always in shadow mode, and under
    /// `enforce` only when some work fits.
    pub(crate) fn requests(&self) -> bool {
        !self.record.sent || !self.fit.is_empty()
    }

    /// Record the request's original forecast, counting the returned claim
    /// with exactly the lookup used to reserve it for the next request.
    pub(crate) fn record_forecast(
        mut self,
        runtime: &OrbitRuntime,
        run_id: &str,
        sent: bool,
        receipt: Option<&AdmissionReceipt>,
    ) -> Result<(), OrbitError> {
        let mut admitted = Vec::new();
        if let Some(receipt) = receipt
            && let Some(request) = runtime.pulled_claim_reservation(receipt)?
        {
            self.accuracy.admit(&request.lookup);
            admitted.push(request.task_id);
        }
        runtime.record_admission_forecast(&AdmissionForecastRecord {
            workspace_id: runtime.workspace_id()?,
            run_id: run_id.to_string(),
            forecast_at: self.accuracy.at,
            mode: self.record.mode,
            cpu_count: self.accuracy.cpu_count,
            memory_total_bytes: self.accuracy.memory_total_bytes,
            points: self.accuracy.into_points(),
            admitted,
            fit_admitted: Vec::new(),
            fit_skipped: Vec::new(),
            pull: Some(PullForecastDecision {
                sent,
                forecast_no_headroom: self.fit.is_empty(),
            }),
        })
    }
}

impl OrbitRuntime {
    /// What this host's load forecast has room for, computed under the host
    /// admission lock; `None` with `machine.auto_concurrency.mode = "off"`,
    /// on a host with no resource profiles, or when the host's load cannot be
    /// read, where pull admission stays as it is without the forecast.
    pub(crate) fn pull_work_fit(
        &self,
        request_id: &str,
    ) -> Result<Option<PullWorkFit>, OrbitError> {
        let settings = self.context.settings().auto_concurrency().clone();
        let enforce = match settings.mode.as_str() {
            ENFORCE => true,
            SHADOW => false,
            _ => return Ok(None),
        };
        let workspace = self.workspace_id()?;
        self.admit_against_forecast(|forecast| {
            let outcome = forecast
                .gap(settings.admit_cpu_percent, settings.admit_memory_percent)
                .and_then(|gap| work_fit(forecast, &gap, &workspace, enforce, &settings.mode));
            let admitted = outcome
                .as_ref()
                .filter(|_| enforce)
                .map_or_else(Vec::new, |fit| {
                    fit.pending
                        .iter()
                        .cloned()
                        .map(|lookup| AdmissionReservationRequest {
                            task_id: pending_pull_reservation_id(request_id),
                            lookup,
                        })
                        .collect()
                });
            Ok(ForecastDecision { outcome, admitted })
        })
    }

    /// Refresh capacity before every send, including a carried unanswered
    /// request whose earlier reservation expired. The request's durable
    /// admissible set cannot change when its identity is replayed.
    pub(crate) fn prepare_pull_capacity(
        &self,
        request: &AdmissionRequest,
    ) -> Result<bool, OrbitError> {
        let settings = self.context.settings().auto_concurrency();
        if settings.mode != ENFORCE {
            return Ok(true);
        }
        let Some(fit) = request.admissible.as_ref() else {
            return Ok(true); // No forecast was available when this request was made.
        };
        let workspace = self.workspace_id()?;
        let pending = pending_pull_reservation_id(&request.request_id);
        self.admit_against_forecast_replacing_at(
            Utc::now(),
            &[(&workspace, &pending)],
            |forecast| {
                let lookups = fit
                    .keys
                    .iter()
                    .map(|key| {
                        ProfileLookup::new(
                            &workspace,
                            &key.work_class,
                            key.complexity.clone(),
                            false,
                        )
                    })
                    .chain(fit.templates.iter().map(|template| {
                        let class = format!("{AUTO_TASK_TAG_PREFIX}{template}");
                        let no_diff = forecast.profiles.profiles.iter().any(|profile| {
                            profile.key.workspace_id == workspace
                                && profile.key.work_class == class
                                && profile.base_class == BaseClass::NoDiff
                        });
                        ProfileLookup::new(&workspace, class, None, no_diff)
                    }))
                    .collect::<Vec<_>>();
                let gap = forecast.gap(settings.admit_cpu_percent, settings.admit_memory_percent);
                if lookups.iter().any(|lookup| {
                    forecast
                        .profiles
                        .lookup(lookup)
                        .profile
                        .as_ref()
                        .is_some_and(|profile| {
                            gap.as_ref().is_some_and(|gap| gap.fit(profile).is_err())
                        })
                }) {
                    // A retry whose placeholder expired waits for capacity instead
                    // of sending an already-priced request into somebody else's gap.
                    return Ok(ForecastDecision {
                        outcome: false,
                        admitted: Vec::new(),
                    });
                }
                Ok(ForecastDecision {
                    outcome: true,
                    admitted: lookups
                        .into_iter()
                        .map(|lookup| AdmissionReservationRequest {
                            task_id: pending.clone(),
                            lookup,
                        })
                        .collect(),
                })
            },
        )
    }

    /// Exchange the placeholder for precise capacity before creating a leaf.
    /// A response delayed beyond the TTL may find the gap filled: retain the
    /// owner's claim locally and wait, never launch it without capacity.
    pub(crate) fn reconcile_pull_capacity(
        &self,
        record: &LocalPullAdmission,
    ) -> Result<bool, OrbitError> {
        match record.phase {
            LocalPullPhase::Idle
            | LocalPullPhase::Refused
            | LocalPullPhase::Settled
            | LocalPullPhase::Settling => {
                self.release_pull_capacity(&record.request)?;
                return Ok(true);
            }
            LocalPullPhase::Claimed => {}
            _ => return Ok(true),
        }
        let Some(receipt) = record.receipt.as_ref() else {
            return Ok(false);
        };
        let Some(request) = self.pulled_claim_reservation(receipt)? else {
            return Ok(false);
        };
        let settings = self.context.settings().auto_concurrency();
        if !matches!(settings.mode.as_str(), ENFORCE | SHADOW) {
            return Ok(true);
        }
        let workspace = self.workspace_id()?;
        let pending = pending_pull_reservation_id(&record.request.request_id);
        let task = request.task_id.clone();
        self.admit_against_forecast_replacing_at(
            Utc::now(),
            &[(&workspace, &pending), (&workspace, &task)],
            |forecast| {
                let fits = settings.mode != ENFORCE
                    || forecast
                        .profiles
                        .lookup(&request.lookup)
                        .profile
                        .as_ref()
                        .is_none_or(|profile| {
                            forecast
                                .gap(settings.admit_cpu_percent, settings.admit_memory_percent)
                                .is_none_or(|gap| gap.fit(profile).is_ok())
                        });
                Ok(ForecastDecision {
                    outcome: fits,
                    admitted: if fits { vec![request] } else { Vec::new() },
                })
            },
        )
    }

    /// Release only the request placeholder; live leaves retain their own
    /// forecast contribution. Unknown transport outcomes keep the placeholder
    /// until a receipt is recovered or the normal five-minute TTL expires.
    pub(crate) fn release_pull_capacity(
        &self,
        request: &AdmissionRequest,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace_id()?;
        let pending = pending_pull_reservation_id(&request.request_id);
        self.release_admission_reservations(&[(&workspace, &pending)])
    }

    fn pulled_claim_reservation(
        &self,
        receipt: &AdmissionReceipt,
    ) -> Result<Option<AdmissionReservationRequest>, OrbitError> {
        let Some(task) = receipt.task.as_ref().filter(|_| receipt.claim.is_some()) else {
            return Ok(None);
        };
        let local = self.get_task(&task.id).ok();
        let tags = local
            .as_ref()
            .map_or(&[][..], |local| local.tags.as_slice());
        let complexity = task
            .complexity
            .or_else(|| local.as_ref().and_then(|local| local.complexity));
        let lookup = ProfileLookup::new(
            self.workspace_id()?,
            work_class(tags),
            complexity.map(|complexity| complexity.as_str().to_string()),
            tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG),
        );
        Ok(Some(AdmissionReservationRequest {
            task_id: task.id.clone(),
            lookup,
        }))
    }
}

/// Fit every kind of work of `workspace` into `gap`, the room under
/// `forecast`, each kind on its own. `None` when the host has no profiles:
/// nothing can be priced.
fn work_fit(
    forecast: &HostLoadForecast,
    gap: &ForecastGap,
    workspace: &str,
    enforce: bool,
    mode: &str,
) -> Option<PullWorkFit> {
    let profiles = &forecast.profiles;
    if profiles.profiles.is_empty() && profiles.class_profiles.is_empty() {
        return None;
    }
    let mut fit = AdmissionWorkFit::default();
    let mut admissible = Vec::new();
    let mut unfit = Vec::new();
    let mut pending = Vec::new();
    let mut judge = |work: String, lookup: ProfileLookup| -> bool {
        let verdict = profiles
            .lookup(&lookup)
            .profile
            .map_or(Ok(None), |profile| gap.fit(&profile));
        match verdict {
            Ok(_) => {
                admissible.push(work);
                pending.push(lookup);
                true
            }
            Err(miss) => {
                unfit.push(PullUnfitWork {
                    work,
                    reason: miss.describe(),
                });
                false
            }
        }
    };
    for class in [NO_DIFF_WORK_CLASS, CHANGE_WORK_CLASS] {
        for complexity in COMPLEXITIES {
            let complexity = complexity.map(|complexity| complexity.as_str().to_string());
            let work = format!("{class}/{}", complexity.as_deref().unwrap_or("unset"));
            let lookup = ProfileLookup::new(workspace, class, complexity.clone(), false);
            if judge(work, lookup) {
                fit.keys.push(AdmissionWorkKey {
                    work_class: class.to_string(),
                    complexity,
                });
            }
        }
    }
    // Profiles are in key order, one per template and workspace.
    let templates = profiles
        .profiles
        .iter()
        .filter(|profile| profile.key.workspace_id == workspace)
        .filter_map(|profile| {
            let template = profile.key.template()?;
            Some((template.to_string(), profile.base_class))
        })
        .collect::<Vec<_>>();
    for (template, base) in templates {
        let lookup = ProfileLookup::new(
            workspace,
            format!("{AUTO_TASK_TAG_PREFIX}{template}"),
            None,
            base == BaseClass::NoDiff,
        );
        if judge(lookup.work_class.clone(), lookup) {
            fit.templates.push(template);
        }
    }
    Some(PullWorkFit {
        accuracy: forecast.accuracy_points(),
        pending,
        fit,
        record: PullAdmissionFit {
            mode: mode.to_string(),
            computed_at: forecast.at,
            sent: enforce,
            admissible,
            unfit,
        },
    })
}
