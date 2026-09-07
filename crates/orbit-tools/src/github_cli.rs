//! The GitHub CLI surface, without the tool registry wrapped around it.
//!
//! The `github.*` builtins are how an *agent* reads CI state: they build a
//! `gh` argv, project its JSON into stable field names, and bound and redact
//! anything log-shaped on the way back out. Engine-private deterministic
//! automation needs exactly the same argv, the same projections, and the same
//! bounding — but it runs on the host, outside any agent sandbox, and must not
//! route through `ToolRegistry`, tool authorization, or an activity allowlist.
//!
//! Rather than let a second copy of the GitHub CLI contract grow in
//! `orbit-engine`, this module re-exports the pieces. `orbit-tools` stays the
//! single owner of what a `gh` invocation looks like and of how its output is
//! made safe to carry; callers here own the policy question of when to run it.
//!
//! Every request builder returns an [`ExecRequest`] with `current_dir: None`.
//! Set it to the repository the query is about before executing — `gh`
//! otherwise resolves the repository from the caller's working directory.

pub use crate::builtin::github::auth::build_exec_request as auth_status_request;
pub use crate::builtin::github::dependabot_alerts::{
    build_code_scanning_request as code_scanning_alerts_request,
    build_exec_request as dependabot_alerts_request,
    build_open_pull_requests_request as dependabot_pull_requests_request,
    build_secret_locations_request as secret_scanning_locations_request,
    build_secret_scanning_request as secret_scanning_alerts_request,
    project_alert as project_dependabot_alert, project_code_scanning_alert,
    project_pull_request as project_dependabot_pull_request, project_secret_location,
    project_secret_scanning_alert,
};
pub use crate::builtin::github::landing::commit_pull_requests as commit_pull_requests_request;
pub use crate::builtin::github::logs::{
    LogReadBounds, LogScope, MAX_EVIDENCE_LINES, RunLogRead, RunLogRequests, SOURCE_JOB_API_LOG,
    SOURCE_RUN_LOG, read_run_log,
};
pub use crate::builtin::github::pr_list::{
    build_exec_request as pr_list_request, project_pull_request,
};
pub use crate::builtin::github::repo::{
    build_exec_request as repo_view_request, project_repo_view,
};
pub use crate::builtin::github::run_list::{build_exec_request as run_list_request, project_run};
pub use crate::builtin::github::run_view::{
    build_exec_request as run_view_request, project_run_view,
};
pub use crate::builtin::github::{
    BoundedLog, CheckoutEvidence, StreamedLog, StreamedLogCollector, bound_log_text, parse_gh_json,
    scan_checkout_evidence, strip_ansi_sequences,
};
