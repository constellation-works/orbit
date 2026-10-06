use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::handoff::HandoffDelivery;
use serde_json::Value;

use crate::context::ClaimExecutionContext;

use super::super::git::git_output;
use super::input::refused;

/// The delivery this claim's ship mode produces. `pr` needs the number the
/// run's own `pr_open` step observed; `local` publishes nothing.
///
/// `pub(super)` so the sibling unit test can hit this seam with a
/// `pr_open`-shaped string without observing Git [ORB-12640].
pub(in crate::executor::automation::vcs) fn delivery(
    context: &ClaimExecutionContext,
    input: &Value,
) -> Result<HandoffDelivery, OrbitError> {
    if let Some(evidence) = input
        .get("no_diff_evidence")
        .filter(|value| !value.is_null())
    {
        return Ok(HandoffDelivery::NoDiff {
            evidence: serde_json::from_value(evidence.clone()).map_err(|error| {
                OrbitError::InvalidInput(format!("invalid no-diff evidence reference: {error}"))
            })?,
        });
    }
    match context.ship_mode.as_str() {
        "local" => Ok(HandoffDelivery::LocalCandidate),
        "pr" => {
            let number = input
                .get("pull_request")
                .and_then(pull_request_number)
                .filter(|number| *number > 0)
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "a pr-mode claim hands off a published pull request; pull_request is \
                         required"
                            .to_string(),
                    )
                })?;
            Ok(HandoffDelivery::PullRequest { number })
        }
        other => Err(refused(format!(
            "claimed leaf ship mode '{other}' has no handoff delivery"
        ))),
    }
}

/// The PR number this step was handed, in either shape the run can produce
/// [ORB-12617] [ORB-12640].
///
/// `pr_open` reports `pr_number` as a *string* — a provider selector rather
/// than a quantity — and an exact step-output template forwards the source
/// JSON type unchanged, so the pr-mode leaf receives a string. Reading only
/// `as_u64` here made every published claimed PR fail at its handoff with
/// "pull_request is required" while the number was sitting right there.
fn pull_request_number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Repository identity for the handoff. A checkout with a remote names it; an
/// owner-local checkout has none to observe, so the owner workspace it was
/// claimed from is the identity of record.
pub(super) fn repository(workspace_path: &Path, fallback: &str) -> String {
    git_output(workspace_path, &["remote", "get-url", "origin"])
        .ok()
        .and_then(|url| slug(&url))
        .unwrap_or_else(|| fallback.to_string())
}

pub(in crate::executor::automation::vcs) fn slug(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let tail = trimmed.rsplit_once(':').map_or(trimmed, |(_, tail)| tail);
    let mut segments = tail.rsplitn(3, '/');
    let name = segments.next()?;
    let owner = segments.next()?;
    (!name.is_empty() && !owner.is_empty()).then(|| format!("{owner}/{name}"))
}
