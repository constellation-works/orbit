use chrono::{Duration, Utc};
use orbit_engine::DispatchError;
use orbit_types::workflow::{DrainAdmissionForecast, GapFitHead};
use serde_json::Value;

use super::super::drains::drain_run_id;
use crate::OrbitRuntime;
use crate::application::admission::auto_admission::AdmissionSelection;
use crate::application::admission::gap_fit::{GapFit, GapFitMode, HeadWait};
use crate::application::admission::pass_error::PassError;
use crate::runtime::host_resource::forecast::{ForecastDecision, ForecastPoints};

/// The waiting reason of a candidate `enforce` gap fit kept out of a pass.
pub(super) const GAP_FIT_REASON: &str = "gap_fit";

/// A pass's wave and, under `shadow` or `enforce`, its gap fit.
pub(super) struct GapFitPass {
    pub(super) selection: AdmissionSelection,
    pub(super) record: Option<DrainAdmissionForecast>,
    /// The forecast the pass decided against, when it built one.
    pub(super) forecast: Option<ForecastPoints>,
    /// The `(workspace, task)` reservations `enforce` wrote for the wave.
    pub(super) reserved: Vec<(String, String)>,
}

/// Give back the reservations a pass wrote for a wave it then failed to
/// admit, and return the pass's failure unchanged.
///
/// Reservations are released by task, which is how the host store keys them.
/// A release that fails is logged and the pass's own failure still decides
/// what happens: a pass that lost only to a busy lock is still deferred, so a
/// contended host store does not end the drain on its first pass. The
/// reservations it could not release only hold admission back, expire with
/// the reservation TTL, and the deferral limit bounds how many such passes
/// can stack them.
pub(super) fn release_unadmitted_reservations(
    runtime: &OrbitRuntime,
    reserved: &[(String, String)],
    error: PassError,
) -> PassError {
    release_logged(runtime, reserved, &error.reason());
    error
}

/// Give back the reservations of `reserved` whose task the admission section
/// did not admit (`admitted`). A release that fails is logged: the pass
/// admitted its wave regardless, and the reservation the store could not drop
/// is replaced by the task's next reservation or expires with its TTL.
pub(super) fn release_refused_reservations(
    runtime: &OrbitRuntime,
    reserved: &[(String, String)],
    admitted: &[String],
) {
    let refused: Vec<(String, String)> = reserved
        .iter()
        .filter(|(_, task)| !admitted.contains(task))
        .cloned()
        .collect();
    release_logged(
        runtime,
        &refused,
        "the admission section refused the candidates",
    );
}

fn release_logged(runtime: &OrbitRuntime, reserved: &[(String, String)], cause: &str) {
    if reserved.is_empty() {
        return;
    }
    let keys: Vec<(&str, &str)> = reserved
        .iter()
        .map(|(workspace, task)| (workspace.as_str(), task.as_str()))
        .collect();
    if let Err(release_error) = release_reservations(runtime, &keys) {
        tracing::warn!(
            target: "orbit.core.job_run",
            cause,
            release_error = %release_error,
            reservations = ?reserved,
            "drain could not release the reservations of a wave it did not admit; they expire with their TTL"
        );
    }
}

fn release_reservations(
    runtime: &OrbitRuntime,
    keys: &[(&str, &str)],
) -> Result<(), orbit_common::OrbitError> {
    #[cfg(feature = "test-support")]
    if let Some(error) = super::release_failure_hook::take() {
        return Err(error);
    }
    runtime.release_admission_reservations(keys)
}

/// Select the wave with gap fit, under the host admission lock.
///
/// The forecast is built, the wave chosen and, in `enforce`, a reservation
/// written for each admitted task, all in one host lock, so another drain on
/// the host counts them before their runs exist. `shadow` reserves nothing
/// and admits what `select(None)` admits, as `off` does. A pass that can
/// admit nothing takes no lock and carries only the starvation guard's head.
///
/// A candidate the admission section refuses afterwards starts no run, so
/// the pass releases its reservation ([`release_refused_reservations`]); left
/// to the TTL, a candidate refused every pass would stack a copy per pass
/// onto the forecast. A section that fails outright admits none of the wave,
/// and the pass releases every reservation it wrote
/// ([`release_unadmitted_reservations`]).
pub(super) fn gap_fit_selection(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    mode: GapFitMode,
    pending: &[String],
    can_admit: bool,
    select: &dyn Fn(Option<&mut GapFit<'_>>) -> AdmissionSelection,
) -> Result<GapFitPass, PassError> {
    let settings = runtime.auto_concurrency_settings();
    let previous = previous_head(runtime, input);
    let head_wait = HeadWait {
        previous: previous.as_ref(),
        max_wait: Duration::minutes(i64::from(settings.head_max_wait_minutes)),
        stuck_passes: settings.head_stuck_passes,
        now: Utc::now(),
    };
    if !can_admit {
        return Ok(GapFitPass {
            selection: select(None),
            record: Some(head_wait.record_without_forecast(mode, pending)),
            forecast: None,
            reserved: Vec::new(),
        });
    }
    let workspace_id = runtime
        .workspace_id()
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let decided = runtime.admit_against_forecast(|forecast| {
        let mut gap_fit = GapFit::new(
            mode,
            forecast,
            settings.admit_cpu_percent,
            settings.admit_memory_percent,
            &workspace_id,
            head_wait,
        );
        let selection = select(Some(&mut gap_fit));
        let (record, reservations) = gap_fit.finish(pending);
        let admitted = match mode {
            GapFitMode::Enforce => reservations,
            GapFitMode::Shadow => Vec::new(),
        };
        let reserved = admitted
            .iter()
            .map(|request| (request.lookup.workspace_id.clone(), request.task_id.clone()))
            .collect::<Vec<_>>();
        Ok(ForecastDecision {
            outcome: (selection, record, forecast.accuracy_points(), reserved),
            admitted,
        })
    });
    match (mode, decided) {
        (GapFitMode::Enforce, Ok((selection, record, forecast, reserved))) => Ok(GapFitPass {
            selection,
            record: Some(record),
            forecast: Some(forecast),
            reserved,
        }),
        (GapFitMode::Shadow, Ok((_, record, forecast, _))) => Ok(GapFitPass {
            selection: select(None),
            record: Some(record),
            forecast: Some(forecast),
            reserved: Vec::new(),
        }),
        (GapFitMode::Enforce, Err(error)) => Err(PassError::store(
            action,
            "admit against the host load forecast",
            error,
        )),
        (GapFitMode::Shadow, Err(error)) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                %error,
                "shadow gap fit could not forecast the host; the pass admits as it would without it"
            );
            Ok(GapFitPass {
                selection: select(None),
                record: None,
                forecast: None,
                reserved: Vec::new(),
            })
        }
    }
}

/// The starvation guard's head as the drain's previous pass recorded it.
fn previous_head(runtime: &OrbitRuntime, input: &Value) -> Option<GapFitHead> {
    let run_id = drain_run_id(input)?;
    runtime
        .read_run_state(run_id)
        .ok()
        .flatten()?
        .pull_drain
        .drain_last_pass?
        .admission_forecast?
        .head
}
