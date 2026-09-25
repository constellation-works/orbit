use axum::Router;
use axum::middleware;
use axum::routing::{get, post, put};

use super::*;

/// Tell long-lived streaming handlers (currently `/api/log/stream`) to close
/// cooperatively. Called once from [`crate::serve::shutdown_signal`] so the bounded
/// graceful-drain deadline in `crate::serve::run_server` rarely has to be relied on
/// (ORB-11246).
pub(crate) fn request_shutdown() {
    log::request_shutdown();
}

pub(crate) fn router() -> Router<crate::state::DashboardState> {
    Router::new()
        .route("/search", get(search::search))
        .route(
            "/tasks",
            get(tasks::list_tasks).post(tasks::create_task_action),
        )
        .route("/tasks/locks", get(tasks::list_task_locks))
        .route(
            "/tasks/completion-by-complexity",
            get(tasks::completion_by_complexity),
        )
        .route("/tasks/all", get(workspaces::list_all_tasks))
        .route("/job-runs/all", get(workspaces::list_all_job_runs))
        .route("/workspaces", get(workspaces::list_workspaces))
        .route(
            "/tasks/:id",
            get(tasks::get_task).patch(tasks::update_task_action),
        )
        .route("/crews", get(crews::list_crews))
        // Config inspection and editing [ORB-12724]. Workspace-scoped like the
        // rest of the API: `?workspace=` selects which `.orbit/config.toml`
        // layers over the global file.
        .route("/config/effective", get(config::get_effective_config))
        .route("/config/file", get(config::get_config_file))
        .route("/config/keys", get(config::get_config_keys))
        .route(
            "/config/keys/:key",
            put(config::put_config_key).delete(config::delete_config_key),
        )
        .route(
            "/config/crews/:name",
            put(config::put_config_crew).delete(config::delete_config_crew),
        )
        .route("/tasks/:id/artifacts/*path", get(tasks::get_task_artifact))
        .route(
            "/automation/:kind/:name/coverage/:batch/evidence",
            get(automation::accepted_evidence),
        )
        .route("/tasks/:id/comments", post(tasks::add_task_comment_action))
        .route("/tasks/:id/approve", post(tasks::approve_task_action))
        .route("/tasks/:id/reject", post(tasks::reject_task_action))
        .route("/tasks/:id/archive", post(tasks::archive_task_action))
        .route(
            "/frictions",
            get(frictions::list_frictions).post(frictions::create_friction_action),
        )
        .route("/frictions/stats", get(frictions::friction_stats))
        .route(
            "/frictions/:id",
            get(frictions::get_friction).patch(frictions::update_friction_action),
        )
        .route(
            "/frictions/:id/resolve",
            post(frictions::resolve_friction_action),
        )
        .route("/jobs", get(jobs::list_jobs))
        .route("/jobs/:id/run", post(jobs::run_job_action))
        .route("/job-runs", get(jobs::list_job_runs))
        .route("/job-runs/:id/resume", post(jobs::resume_job_run_action))
        .route("/workflows/ship", post(runs::ship_workflow_action))
        .route("/workflows/auto", post(runs::auto_drain_workflow_action))
        .route("/workflows/auto/stop", post(runs::auto_drain_stop_action))
        .route("/workflows/auto/readiness", get(runs::auto_drain_readiness))
        // Distributed-drain claim provenance and the owner's handoff actions
        // [ORB-12516]. There is no dedicated distributed tab: these feed the
        // existing task and run views, so the incomplete feature gains no
        // public navigation entry of its own.
        .route("/distributed/claims", get(distributed::list_claims))
        .route(
            "/distributed/handoffs/:id/approve",
            post(distributed::approve_handoff_action),
        )
        .route(
            "/distributed/handoffs/:id/revoke",
            post(distributed::revoke_handoff_action),
        )
        .route(
            "/distributed/claims/:id/recover",
            post(distributed::recover_claim_action),
        )
        .route("/runs/:id", get(runs::get_run))
        .route("/runs/:id/cancel", post(runs::cancel_run_action))
        .route("/runs/:id/replay", post(runs::replay_run_action))
        .route("/runs/:id/events", get(runs::list_run_events))
        .route("/runs/:id/logs", get(runs::list_run_logs))
        .route("/audit", get(audit::list_audit))
        .route("/log", get(log::get_log))
        .route("/log/stream", get(log::stream_log))
        .route("/audit/summary", get(audit::audit_summary))
        .route("/audit/incidents", get(incidents::list_failure_incidents))
        .route("/routines", get(routines::list_routine_health))
        .route("/routines/toggle", post(routines::toggle_routine))
        .route("/routines/clock", post(routines::control_clock))
        .route("/auto-tasks", get(auto_tasks::list_auto_tasks))
        .route("/auto-tasks/toggle", post(auto_tasks::toggle_auto_task))
        .route("/auto-tasks/mint", post(auto_tasks::mint_auto_task))
        .route("/scoreboard", get(scoreboard::scoreboard))
        .route("/metrics/knowledge", get(metrics::knowledge_metrics))
        .route("/metrics/activity", get(metrics::activity_metrics))
        .route("/metrics/tools", get(metrics::tool_metrics))
        .route("/metrics/task/:id", get(metrics::task_metrics))
        .route("/metrics/orchestrators", get(metrics::orchestrator_metrics))
        .route(
            "/metrics/reliability",
            get(reliability::pipeline_reliability),
        )
        .route(
            "/metrics/invocations",
            get(metrics::invocation_metrics).post(metrics::ingest_invocation),
        )
        .route(
            "/diagnostics/metrics",
            get(diagnostics::list_diagnostics_metrics),
        )
        .route(
            "/diagnostics/errors",
            get(diagnostics::list_diagnostics_errors),
        )
        .route(
            "/diagnostics/friction",
            get(diagnostics::list_diagnostics_friction),
        )
        .route(
            "/diagnostics/implement_one",
            get(diagnostics::diagnostics_implement_one),
        )
        .route("/diagnostics/denials", get(denials::list_denials))
        // Installed plugins and their declared panels [§4.7]. Read-only: a
        // panel source is a `read_only` tool, and nothing here enables,
        // disables or configures a plugin — that stays on the CLI, where the
        // grant decision is made.
        .route("/plugins", get(plugins::list_plugins))
        .route(
            "/plugins/:namespace/panels/:panel",
            get(plugins::read_panel),
        )
        .layer(middleware::map_response(json_client_error))
        .layer(middleware::from_fn(require_localhost_origin))
        // Outer so Host/Origin 403s and handler JSON both carry nosniff.
        .layer(middleware::map_response(nosniff_json_responses))
}
