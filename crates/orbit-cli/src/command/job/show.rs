use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::support::{job_catalog_to_json_with_last_run, write_v2_step};

#[derive(Args)]
pub struct JobShowArgs {
    pub job_id: String,
}

impl Execute for JobShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let job = runtime.show_job_catalog_entry(&self.job_id)?;
        let doc = job_catalog_to_json_with_last_run(&job, None);

        use std::fmt::Write as _;
        let mut out = String::new();
        use crate::output::color::{Domain, bold, text};
        let _ = writeln!(out, "{} {}", bold("Job ID:"), job.job_id.as_str());
        let _ = writeln!(out, "{} {}", bold("Kind:"), job.kind());
        let _ = writeln!(
            out,
            "{} {}",
            bold("State:"),
            text(&job.state().to_string(), Domain::JobState)
        );
        let max_active_runs = match job.max_active_runs() {
            0 => "unlimited".to_string(),
            limit => limit.to_string(),
        };
        let _ = writeln!(out, "{} {}", bold("Max Active Runs:"), max_active_runs);
        let _ = writeln!(out, "{} {}", bold("Path:"), job.path.display());
        if let Some(default_input) = job.default_input() {
            let rendered = serde_json::to_string(default_input)
                .unwrap_or_else(|_| "<invalid-json>".to_string());
            let _ = writeln!(out, "{} {}", bold("Default Input:"), rendered);
        }
        let _ = writeln!(out, "{} {}", bold("Steps:"), job.spec.steps.len());
        for (i, step) in job.spec.steps.iter().enumerate() {
            let _ = writeln!(out, "  {}:", bold(&format!("Step {}", i + 1)));
            write_v2_step(step, 4, &mut out);
        }
        Ok(Payload::detail(doc, out).into())
    }
}
