use orbit_core::JobRun;
use orbit_core::application::job::{JobCatalogEntry, JobCatalogFilter};
use orbit_types::workflow::{
    ActivityV2Spec, BackoffStrategy, FanInSpec, JobKind, JobV2Step, JobV2StepBody, JoinMode,
    RetrySpec,
};
use serde_json::{Value, json};

pub(super) fn format_last_run(last_run: Option<&JobRun>) -> String {
    match last_run {
        None => "never".to_string(),
        Some(run) => {
            let ts = run
                .finished_at
                .or(run.started_at)
                .unwrap_or(run.scheduled_at);
            format!("{} {}", run.state, ts.format("%Y-%m-%dT%H:%M:%SZ"))
        }
    }
}

pub(super) fn job_catalog_filter(
    include_disabled: bool,
    kind: Option<JobKind>,
) -> JobCatalogFilter {
    match kind {
        Some(kind) => JobCatalogFilter::Kind(kind),
        None if include_disabled => JobCatalogFilter::All,
        None => JobCatalogFilter::WorkflowsOnly,
    }
}

pub(super) fn job_catalog_target_summary(job: &JobCatalogEntry) -> (String, String) {
    job.spec
        .steps
        .first()
        .map(v2_step_target_summary)
        .unwrap_or_else(|| ("-".to_string(), "-".to_string()))
}

pub(super) fn job_catalog_to_json_with_last_run(
    job: &JobCatalogEntry,
    last_run: Option<&JobRun>,
) -> Value {
    let mut value = json!({
        "job_id": job.job_id.clone(),
        "kind": job.kind().to_string(),
        "state": job.state().to_string(),
        "default_input": job.spec.default_input,
        "max_active_runs": job.spec.max_active_runs,
        "steps": job.spec.steps.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
        "path": job.path.display().to_string(),
    });
    value["last_run_state"] = last_run
        .map(|r| serde_json::Value::String(r.state.to_string()))
        .unwrap_or(serde_json::Value::Null);
    value["last_run_at"] = last_run
        .and_then(|r| r.finished_at.or(r.started_at).or(Some(r.scheduled_at)))
        .map(|ts| serde_json::Value::String(ts.to_rfc3339()))
        .unwrap_or(serde_json::Value::Null);
    value
}

pub(super) fn job_catalog_to_signal_json(job: &JobCatalogEntry) -> Value {
    let (_, target_id) = job_catalog_target_summary(job);
    json!({
        "job_id": job.job_id.clone(),
        "target_id": target_id,
        "state": job.state().to_string(),
    })
}

fn job_v2_step_to_json(step: &JobV2Step) -> Value {
    let mut value = json!({
        "id": step.id.clone(),
        "when": step.when,
        "retry": step.retry,
    });
    match &step.body {
        JobV2StepBody::TargetRef(target) => {
            value["body"] = json!({
                "kind": "target_ref",
                "target": target.target.clone(),
                "default_input": target.default_input,
                "timeout_seconds": target.timeout_seconds,
                "session": target.session,
            });
        }
        JobV2StepBody::Target(target) => {
            value["body"] = json!({
                "kind": "target",
                "default_input": target.default_input,
                "timeout_seconds": target.timeout_seconds,
                "session": target.session,
                "spec": target.spec,
            });
        }
        JobV2StepBody::Parallel { parallel } => {
            value["body"] = json!({
                "kind": "parallel",
                "join": parallel.join,
                "branches": parallel.branches.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
            });
        }
        JobV2StepBody::FanOut { fan_out, fan_in } => {
            value["body"] = json!({
                "kind": "fan_out",
                "items": fan_out.items,
                "max_workers": fan_out.max_workers,
                "worker": job_v2_step_to_json(&fan_out.worker),
                "fan_in": fan_in,
            });
        }
        JobV2StepBody::Loop { loop_ } => {
            value["body"] = json!({
                "kind": "loop",
                "max_iterations": loop_.max_iterations,
                "break_when": loop_.break_when,
                "steps": loop_.steps.iter().map(job_v2_step_to_json).collect::<Vec<_>>(),
            });
        }
    }
    value
}

pub(super) fn write_v2_step(step: &JobV2Step, indent: usize, out: &mut String) {
    use crate::output::color::bold;
    use std::fmt::Write as _;

    let pad = " ".repeat(indent);
    let _ = writeln!(out, "{pad}{} {}", bold("ID:"), step.id.as_str());
    if let Some(when) = &step.when {
        let _ = writeln!(out, "{pad}{} {}", bold("When:"), when);
    }
    if let Some(retry) = &step.retry {
        let _ = writeln!(out, "{pad}{} {}", bold("Retry:"), format_retry(retry));
    }
    match &step.body {
        JobV2StepBody::TargetRef(target) => {
            let _ = writeln!(
                out,
                "{pad}{} {}",
                bold("Target Ref:"),
                target.target.as_str()
            );
            if let Some(session) = &target.session {
                let _ = writeln!(out, "{pad}{} {}", bold("Session:"), session);
            }
            let _ = writeln!(
                out,
                "{pad}{} {}",
                bold("Timeout (s):"),
                target.timeout_seconds
            );
        }
        JobV2StepBody::Target(target) => {
            match &target.spec {
                ActivityV2Spec::AgentLoop(spec) => {
                    let _ = writeln!(out, "{pad}{} agent_loop", bold("Activity Type:"));
                    let _ = writeln!(out, "{pad}{} {}", bold("Provider:"), spec.provider.as_str());
                    if let Some(model) = &spec.model {
                        let _ = writeln!(out, "{pad}{} {}", bold("Model:"), model);
                    }
                }
                ActivityV2Spec::Deterministic(spec) => {
                    let _ = writeln!(out, "{pad}{} deterministic", bold("Activity Type:"));
                    let _ = writeln!(out, "{pad}{} {}", bold("Action:"), spec.action.as_str());
                }
            }
            if let Some(session) = &target.session {
                let _ = writeln!(out, "{pad}{} {}", bold("Session:"), session);
            }
            let _ = writeln!(
                out,
                "{pad}{} {}",
                bold("Timeout (s):"),
                target.timeout_seconds
            );
        }
        JobV2StepBody::Parallel { parallel } => {
            let _ = writeln!(out, "{pad}{} parallel", bold("Body:"));
            let _ = writeln!(
                out,
                "{pad}{} {}",
                bold("Join:"),
                format_join_mode(&parallel.join)
            );
            let _ = writeln!(
                out,
                "{pad}{} {}",
                bold("Branches:"),
                parallel.branches.len()
            );
            for branch in &parallel.branches {
                write_v2_step(branch, indent + 2, out);
            }
        }
        JobV2StepBody::FanOut { fan_out, fan_in } => {
            let _ = writeln!(out, "{pad}{} fan_out", bold("Body:"));
            let _ = writeln!(out, "{pad}{} {}", bold("Items:"), fan_out.items.as_str());
            let _ = writeln!(out, "{pad}{} {}", bold("Max Workers:"), fan_out.max_workers);
            let _ = writeln!(out, "{pad}{} {}", bold("Fan In:"), format_fan_in(fan_in));
            write_v2_step(&fan_out.worker, indent + 2, out);
        }
        JobV2StepBody::Loop { loop_ } => {
            let _ = writeln!(out, "{pad}{} loop", bold("Body:"));
            let _ = writeln!(
                out,
                "{pad}{} {}",
                bold("Max Iterations:"),
                loop_.max_iterations
            );
            if let Some(break_when) = &loop_.break_when {
                let _ = writeln!(out, "{pad}{} {}", bold("Break When:"), break_when);
            }
            for nested in &loop_.steps {
                write_v2_step(nested, indent + 2, out);
            }
        }
    }
}

fn v2_step_target_summary(step: &JobV2Step) -> (String, String) {
    match &step.body {
        JobV2StepBody::TargetRef(target) => ("activity_ref".to_string(), target.target.clone()),
        JobV2StepBody::Target(target) => match &target.spec {
            ActivityV2Spec::AgentLoop(spec) => (
                "agent_loop".to_string(),
                spec.model
                    .clone()
                    .unwrap_or_else(|| spec.provider.as_str().to_string()),
            ),
            ActivityV2Spec::Deterministic(spec) => {
                ("deterministic".to_string(), spec.action.clone())
            }
        },
        JobV2StepBody::Parallel { .. } => ("parallel".to_string(), step.id.clone()),
        JobV2StepBody::FanOut { .. } => ("fan_out".to_string(), step.id.clone()),
        JobV2StepBody::Loop { .. } => ("loop".to_string(), step.id.clone()),
    }
}

pub(super) fn format_join_mode(mode: &JoinMode) -> String {
    match mode {
        JoinMode::All => "all".to_string(),
        JoinMode::Any => "any".to_string(),
        JoinMode::Quorum { n } => format!("quorum({n})"),
    }
}

pub(super) fn format_fan_in(fan_in: &FanInSpec) -> String {
    let join = format_join_mode(&fan_in.join);
    if let Some(collect) = &fan_in.collect {
        format!("join={join} collect={collect}")
    } else {
        format!("join={join}")
    }
}

pub(super) fn format_retry(retry: &RetrySpec) -> String {
    let strategy = match retry.backoff_strategy {
        BackoffStrategy::Exponential => "exponential",
        BackoffStrategy::Linear => "linear",
    };
    format!(
        "max_attempts={} initial_backoff_ms={} backoff_cap_ms={} strategy={strategy}",
        retry.max_attempts, retry.initial_backoff_ms, retry.backoff_cap_ms
    )
}
