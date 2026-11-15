//! `file_ci_failure_tasks` — turn one CI evidence snapshot into quarantined
//! proposed tasks.
//!
//! The snapshot arrives from the host-owned `collect_ci_evidence` step, which
//! ran `gh` outside any agent sandbox. Filing combines that JSON with workspace
//! task/run evidence and Git applicability: cluster current failures by
//! root cause, reuse open owners or completed owners with verified repair
//! evidence, and file what is left as `proposed` bug tasks with inline evidence.
//! The CI sweep then pilots and revalidates those tasks before a
//! separate admission boundary may expose them to backlog auto-drain.
//!
//! A filed task deliberately has no `required_tools`: the pilot and eventual
//! implementation never have to query GitHub, because the original evidence
//! is already in the description. It is not executable until a successful
//! pilot has applied valid selectors, found the failure relevant to current
//! integration code, and exercised explicit sweep promotion authority.
//!
//! # Two keys, on purpose
//!
//! `cluster_key` includes the commit the runner actually tested, so one
//! regression observed across a push run and a pull-request run of the *same*
//! commit collapses into one task instead of two.
//!
//! Ordinary `failure_key` tags omit the commit to keep a still-open repair
//! across branch advances. Concrete failing-test keys omit job and step
//! wrappers so equal test signatures retain one owner across job ordering.
//! Proven compiler causes instead include the exact
//! diagnostic set, source locations and observed checkout, omitting job and
//! workflow wrappers. That conservative proof consolidates cross-job failures
//! without conflating different compiler operands or source revisions. Shipped
//! per-job test tags remain exact owners; legacy compiler tags require the
//! same immutable supplying evidence.
//! A separate complete code/message/path identity reuses an open CI-sweep
//! compiler owner across checkout and coordinate changes. Each new observation
//! is appended to its comments with the exact key preserved as evidence.

mod assess;
mod branch;
mod entry;
mod file_clusters;
mod report;
mod snapshot;

pub(crate) use entry::file_ci_failure_tasks;

/// Wire contract with `collect_ci_evidence` (`orbit-engine`'s
/// `executor::automation::ci`), also stated in both activity assets' schemas.
/// The endings must never collapse into one another, and none of them is a
/// CI pass.
const OUTCOME_CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
const OUTCOME_NO_CURRENT_FAILURE: &str = "no_current_failure";
const OUTCOME_CURRENT_FAILURES: &str = "current_failures";
/// [ORB-15517] Current failures exist, but every one waits on evidence a
/// later sweep can complete; nothing was filed and nothing failed.
const OUTCOME_DEFERRED: &str = "deferred";
/// Collection's hint that GitHub rate-limited its reads; its snapshot carries
/// `rate_limit` with the not-before, and filing reports [`OUTCOME_DEFERRED`].
const OUTCOME_HINT_RATE_LIMITED: &str = "rate_limited";

/// Snapshot schema this step knows how to read.
const SUPPORTED_SCHEMA_VERSION: u64 = 2;

/// Prefix of the dedupe tag, completed by the failure key.
pub(crate) use orbit_types::task::CI_FAILURE_KEY_TAG_PREFIX;
/// Title prefix on every task this step files, so a sweep-filed task is
/// identifiable in a backlog listing without reading its tags.
pub(super) const CI_FAILURE_SWEEP_TITLE_PREFIX: &str = "[ci-failure-sweep] ";
const DEFAULT_MAX_TASKS: u64 = 5;
const MAX_MAX_TASKS: u64 = 20;
/// Log bytes carried into a task description. `collect_ci_evidence` has already
/// bounded and redacted the excerpt; this is a second, tighter bound so a
/// description stays a readable brief.
pub(super) const DESCRIPTION_LOG_BYTES: usize = 4_000;
/// Runs listed per cluster in the description.
pub(super) const MAX_LISTED_RUNS: usize = 6;
