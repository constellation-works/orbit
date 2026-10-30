//! Bounded retry diagnostics bound to one provider failure and trusted source.
use orbit_common::OrbitError;
use orbit_tools::github_cli::{SOURCE_JOB_API_LOG, SOURCE_RUN_LOG, strip_ansi_sequences};
use serde_json::{Value, json};
use std::path::Path;

use super::super::super::ci::log_signature::log_payload;
use super::super::completion_deadline::Deadline;
use super::{
    ci::ci_job_log,
    input::{number, text},
    prerequisite::unreadable,
};

pub(super) struct RetryJobLog {
    pub(super) text: String,
    pub(super) proof: Value,
}

/// Display truncation never proves or disproves source/command completeness.
/// The collector owns those proofs; this adapter adds provider/source binding.
pub(super) fn complete_job_log(
    deadline: Deadline,
    workspace: &Path,
    run_id: u64,
    job: &Value,
    source: &str,
) -> Result<RetryJobLog, OrbitError> {
    let read = ci_job_log(
        &json!({"workspace_path":workspace,"run_id":run_id.to_string(),"job_id":number(job,"id",unreadable)?.to_string(),"deadline_ms":deadline.0}),
    )?;
    if read["source_complete"] != true || !read["fallback_error"].is_null() {
        return Err(unreadable(
            "retry diagnostic source is incomplete or unreadable; recover the complete job log",
        ));
    }
    let steps = job["steps"]
        .as_array()
        .ok_or_else(|| unreadable("retry provider steps"))?;
    let failed = steps
        .iter()
        .filter(|step| step["conclusion"] == "failure")
        .collect::<Vec<_>>();
    let [step] = failed.as_slice() else {
        return Err(unreadable("retry requires one known failed provider step"));
    };
    let name = text(step, "name", unreadable)?;
    if name == "UNKNOWN STEP"
        || steps
            .iter()
            .filter(|candidate| candidate["name"] == name)
            .count()
            != 1
    {
        return Err(unreadable("retry failed step identity is ambiguous"));
    }
    let (kind, selected) = if let Some(unit) = read["diagnostic"].as_str() {
        ("complete_command", unit)
    } else if read["failure_regions"].is_object() {
        let regions = &read["failure_regions"];
        if regions["kind"] != "runner_failure_regions"
            || regions["command_complete"] != true
            || regions["selection_complete"] != true
        {
            return Err(unreadable(
                "retry failure-region selection is incomplete; recover complete selected failures",
            ));
        }
        (
            "selected_failure_regions",
            text(regions, "text", unreadable)?,
        )
    } else if read["truncated"] == false {
        ("complete_failed_step", text(&read, "log", unreadable)?)
    } else {
        return Err(unreadable(
            "shortened retry display has no complete command or selected failure regions",
        ));
    };
    if selected.trim().is_empty() {
        return Err(unreadable("empty retry diagnostic"));
    }
    let binding = match read["source"].as_str() {
        Some(SOURCE_RUN_LOG) => {
            for line in selected.lines() {
                let mut columns = line.splitn(3, '\t');
                if columns.next() != Some(text(job, "name", unreadable)?)
                    || columns.next() != Some(name)
                    || columns.next().is_none()
                {
                    return Err(unreadable(
                        "retry diagnostic columns do not bind the provider job and failed step",
                    ));
                }
            }
            json!({"kind":"provider_columns","step":step})
        }
        Some(SOURCE_JOB_API_LOG) => {
            let jobs = read["source_jobs"]
                .as_array()
                .ok_or_else(|| unreadable("fallback source jobs"))?;
            let [observed] = jobs.as_slice() else {
                return Err(unreadable("fallback log has no unique source job"));
            };
            if observed["job_id"] != job["id"]
                || ["name", "status", "conclusion"]
                    .iter()
                    .any(|field| observed[field] != job[field])
            {
                return Err(unreadable("fallback source differs from the provider job"));
            }
            // A full raw log alone has no step identity. Require the collector's
            // unique completed failed command, then bind its exact script to
            // one unchanged workflow step. Timestamps/records are not a binding.
            if kind == "complete_failed_step" {
                return Err(unreadable(
                    "raw fallback needs a unique complete failing command; step columns are unavailable",
                ));
            }
            let display = read["diagnostic_command"].as_str().ok_or_else(|| unreadable("raw fallback lacks a complete unique failing-command display; recover step-column logs"))?;
            let mut binding = workflow_command_binding(source, job, step, display)?;
            binding["provider_timing"] = raw_step_timing(
                job,
                step,
                display,
                selected,
                kind == "selected_failure_regions",
            )?;
            binding
        }
        _ => return Err(unreadable("unknown retry diagnostic source")),
    };
    let mut regions = read["failure_regions"].clone();
    if let Some(regions) = regions.as_object_mut() {
        regions.remove("text");
    }
    let proof = json!({"kind":kind,"source":read["source"],"source_complete":true,"display_truncated":read["truncated"],"source_bytes":read["source_bytes"],"run_id":run_id,"job_id":job["id"],"source_jobs":read["source_jobs"],"diagnostic_command":read["diagnostic_command"],"failure_regions":regions,"binding":binding});
    // Keep raw bounded evidence for audit and for the command/quoted-record
    // classifier. Removing prefixes before classification can reinterpret tabs.
    Ok(RetryJobLog {
        text: selected.to_owned(),
        proof,
    })
}

/// Raw runner timestamps are useful only after the API read binds the source
/// job. Use their first physical prefix, never a timestamp quoted in output.
/// Provider timestamps have second precision: overlapping seconds cannot
/// establish attribution. Require a witness in the failed step alone.
fn raw_step_timing(
    job: &Value,
    failed: &Value,
    display: &str,
    selected: &str,
    regions: bool,
) -> Result<Value, OrbitError> {
    use chrono::{DateTime, Duration};
    let steps = job["steps"]
        .as_array()
        .ok_or_else(|| unreadable("raw fallback provider steps"))?;
    let mut windows = Vec::new();
    for step in steps.iter().filter(|step| step["conclusion"] != "skipped") {
        let parse = |key| {
            step[key].as_str().and_then(|value| DateTime::parse_from_rfc3339(value).ok()).ok_or_else(|| unreadable("raw fallback needs complete provider step timestamps; recover step-column logs"))
        };
        let start = parse("started_at")?;
        let end = parse("completed_at")?;
        if end < start {
            return Err(unreadable(
                "raw fallback provider step time window is invalid",
            ));
        }
        let upper = end
            .checked_add_signed(Duration::seconds(1))
            .ok_or_else(|| unreadable("raw fallback provider time overflow"))?;
        windows.push((number(step, "number", unreadable)?, start, upper));
    }
    let failed_number = number(failed, "number", unreadable)?;
    let target = windows
        .iter()
        .find(|(number, _, _)| *number == failed_number)
        .ok_or_else(|| unreadable("raw fallback failed step time window"))?;
    let mut witness = None;
    for line in display.lines().chain(selected.lines()) {
        // These are collector-generated gaps in selected regions, not source
        // lines. Every retained physical source line must carry its timestamp.
        if regions && line.starts_with("[... ") && line.ends_with(" ...]") {
            continue;
        }
        let timestamp = line.split_once(' ').and_then(|(prefix, _)| DateTime::parse_from_rfc3339(prefix).ok()).ok_or_else(|| unreadable("raw fallback command has no physical runner timestamp; recover step-column logs"))?;
        if timestamp < target.1 || timestamp >= target.2 {
            return Err(unreadable(
                "raw fallback command falls outside the provider failed step; recover step-column logs",
            ));
        }
        if windows
            .iter()
            .filter(|(_, start, end)| timestamp >= *start && timestamp < *end)
            .count()
            == 1
        {
            witness = Some(timestamp);
        }
    }
    let witness = witness.ok_or_else(|| unreadable("raw fallback step timing is ambiguous at provider timestamp precision; recover step-column logs"))?;
    Ok(
        json!({"failed_step":failed_number,"started_at":failed["started_at"],"completed_at":failed["completed_at"],"exclusive_witness":witness.to_rfc3339()}),
    )
}

fn workflow_command_binding(
    source: &str,
    job: &Value,
    step: &Value,
    display: &str,
) -> Result<Value, OrbitError> {
    let workflow: Value =
        serde_yaml::from_str(source).map_err(|_| unreadable("fallback workflow source"))?;
    let jobs = workflow["jobs"]
        .as_object()
        .ok_or_else(|| unreadable("fallback workflow jobs"))?;
    let mut matching = Vec::new();
    for (key, candidate) in jobs {
        if super::workflow_dependency::job_names(key, candidate)?
            .contains(&text(job, "name", unreadable)?.to_string())
        {
            matching.push((key, candidate));
        }
    }
    let [(key, source_job)] = matching.as_slice() else {
        return Err(unreadable(
            "raw fallback job has no unique trusted workflow mapping",
        ));
    };
    let source_steps = source_job["steps"]
        .as_array()
        .ok_or_else(|| unreadable("fallback workflow steps"))?;
    let matches = source_steps
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate["name"] == step["name"])
        .collect::<Vec<_>>();
    let [(index, producer)] = matches.as_slice() else {
        return Err(unreadable(
            "raw fallback failed step has no unique named workflow mapping",
        ));
    };
    let command = text(producer, "run", unreadable)?.trim_end_matches(['\r', '\n']);
    if command.trim().is_empty() || command.contains("${{") || producer.get("uses").is_some() {
        return Err(unreadable(
            "raw fallback command uses unsupported expressions or an action; recover step-column logs",
        ));
    }
    // A provider number is not a workflow index (runner setup and action post
    // steps add rows). Bind by its unique name; retain both identities.
    number(step, "number", unreadable)?;
    let lines = display
        .lines()
        .map(|line| strip_ansi_sequences(log_payload(line)))
        .collect::<Vec<_>>();
    let header = lines
        .first()
        .and_then(|line| line.strip_prefix("##[group]Run "));
    let shell = lines.iter().position(|line| line.starts_with("shell: "));
    let Some(shell) = shell else {
        return Err(unreadable(
            "raw fallback command display lacks runner shell metadata",
        ));
    };
    if let Some(declared) = producer["shell"].as_str()
        && (declared.contains("${{")
            || (declared.starts_with('/') && lines[shell] != format!("shell: {declared}")))
    {
        return Err(unreadable(
            "raw fallback shell differs from the trusted failed step; recover step-column logs",
        ));
    }
    if shell == 0
        || lines.last().map(String::as_str) != Some("##[endgroup]")
        || header != command.lines().next()
        || lines[1..shell].join("\n") != command
    {
        return Err(unreadable(
            "raw fallback command differs from its trusted failed step; recover step-column logs",
        ));
    }
    // Equal scripts in distinct steps cannot authenticate which one failed.
    if source_steps
        .iter()
        .filter(|candidate| {
            candidate["run"]
                .as_str()
                .map(|run| run.trim_end_matches(['\r', '\n']))
                == Some(command)
        })
        .count()
        != 1
    {
        return Err(unreadable(
            "raw fallback command maps to multiple workflow steps; recover step-column logs",
        ));
    }
    Ok(
        json!({"kind":"trusted_workflow_command","job_key":key,"workflow_step_index":index,"provider_step":step}),
    )
}
