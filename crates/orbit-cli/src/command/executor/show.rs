use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};

use crate::command::{CommandOut, Execute, Payload};

use super::support::executor_def_json;

#[derive(Args)]
pub struct ExecutorShowArgs {
    pub name: String,
    #[arg(long)]
    pub json: bool,
}

impl Execute for ExecutorShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let def = runtime.get_executor_def(&self.name)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!("executor not found: {}", self.name))
        })?;
        let doc = executor_def_json(&def);

        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "Name:      {}", def.name);
        let _ = writeln!(out, "Type:      {}", def.executor_type);
        let _ = writeln!(
            out,
            "Sandbox:   {}",
            def.sandbox.map_or("unspecified", |kind| kind.as_str())
        );
        if let Some(ref cmd) = def.command {
            let _ = writeln!(out, "Command:   {cmd}");
        }
        if !def.args.is_empty() {
            let _ = writeln!(out, "Args:      {}", def.args.join(" "));
        }
        if let Some(ref fmt) = def.stdout_format {
            let _ = writeln!(out, "Stdout:    {fmt}");
        }
        if let Some(timeout) = def.timeout_seconds {
            let _ = writeln!(out, "Timeout:   {timeout}s");
        }
        if !def.env.is_empty() {
            let _ = writeln!(out, "Env:");
            for (k, v) in &def.env {
                let _ = writeln!(out, "  {k}={v}");
            }
        }
        if let Some(created_at) = def.created_at {
            let _ = writeln!(out, "Created:   {created_at}");
        }
        if let Some(updated_at) = def.updated_at {
            let _ = writeln!(out, "Updated:   {updated_at}");
        }
        Ok(Payload::detail(doc, out).into())
    }
}
