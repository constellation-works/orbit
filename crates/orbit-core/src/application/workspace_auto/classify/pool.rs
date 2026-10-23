use orbit_engine::DispatchError;
use serde_json::Value;

/// Wait before re-listing when the backlog has admissible work but every slot
/// is occupied. This is the latency a freed slot sits idle, so it is much
/// shorter than the idle wait.
pub(super) const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;

/// Wait before re-listing when nothing is admissible at all. Long, because the
/// only thing that can change is a task arriving or a live child finishing.
pub(super) const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

/// Default and ceiling for the backlog prefix one iteration examines, matching
/// `list_backlog_tasks`'s `max_tasks` bound.
pub(in super::super) const DEFAULT_CANDIDATE_POOL: u64 = 50;
const MAX_CANDIDATE_POOL: u64 = 500;

/// A numeric loop input, tolerating the string a template renders. A step's
/// `default_input` value goes through the template engine, so `5` arrives as
/// `"5"`; an input the caller omitted renders as an empty string rather than
/// JSON `null`, which is the default case.
pub(in super::super) fn templated_u64(
    action: &str,
    input: &Value,
    name: &str,
    default: u64,
) -> Result<u64, DispatchError> {
    let Some(raw) = input.get(name) else {
        return Ok(default);
    };
    match raw {
        Value::Null => Ok(default),
        Value::Number(number) => number.as_u64().ok_or_else(|| {
            DispatchError::action_failed(action, format!("`{name}` must be a whole number"))
        }),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(default);
            }
            text.parse::<u64>().map_err(|err| {
                DispatchError::action_failed(
                    action,
                    format!("`{name}` '{text}' is not a number: {err}"),
                )
            })
        }
        other => Err(DispatchError::action_failed(
            action,
            format!("`{name}` must be a number, got {other}"),
        )),
    }
}

/// How many backlog candidates one iteration may expand lock footprints for.
///
/// Mirrors `list_backlog_tasks`'s `max_tasks` bound — same default, same
/// ceiling — because the two describe the same scan. It goes through
/// [`templated_u64`] rather than `as_u64` because the drain job templates the
/// value, which renders `50` as `"50"`.
pub(in super::super) fn candidate_pool_limit(
    action: &str,
    input: &Value,
) -> Result<usize, DispatchError> {
    let requested = templated_u64(action, input, "max_tasks", DEFAULT_CANDIDATE_POOL)?;
    Ok(usize::try_from(requested.clamp(1, MAX_CANDIDATE_POOL)).unwrap_or(usize::MAX))
}
