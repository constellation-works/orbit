use clap::Args;
use orbit_core::bootstrap::init::{InitOptions, init_global};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry::global_orbit_dir;
use orbit_registry::{
    HostIdentityOutcome, NewHostIdentity, ensure_host_identity, os_hostname,
    validate_new_task_prefix,
};
use std::io::{self, BufRead, Write};
use std::path::Path;

use super::collect_config_seed_for_init;
use crate::command::{CommandOut, CommandOutput, Execute};

#[derive(Args)]
#[command(about = "Initialize the global Orbit root (~/.orbit)")]
pub struct InitCommand {
    /// Reset the global Orbit root (~/.orbit/) to shipped defaults before
    /// initialization, including executor sandbox settings
    #[arg(long)]
    pub force: bool,

    /// Skip interactive prompts. config.toml is still seeded from detected
    /// agent surfaces, but a CI runner that pipes nothing into stdin will not
    /// hang.
    #[arg(long)]
    pub non_interactive: bool,

    /// Operator-chosen host name for this machine's identity. Used only when
    /// no identity exists yet (first init). Required with --non-interactive on
    /// a fresh host; interactively, the OS hostname is the default.
    #[arg(long)]
    pub host_name: Option<String>,

    /// Immutable task-id namespace for this machine (2-5 uppercase ASCII
    /// letters). Required on first init; reserved artifact namespaces cannot
    /// be chosen.
    #[arg(long, value_name = "PREFIX")]
    pub task_prefix: Option<String>,
}

impl Execute for InitCommand {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        {
            self.run(None)?;
            Ok(CommandOutput::Silent)
        }
    }
}

impl InitCommand {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        {
            self.run(root_override)?;
            Ok(CommandOutput::Silent)
        }
    }

    fn run(self, root_override: Option<&Path>) -> Result<(), OrbitError> {
        let config_seed =
            collect_config_seed_for_init(root_override, self.force, self.non_interactive)?;
        let result = init_global(
            root_override,
            InitOptions {
                force: self.force,
                refresh_defaults: true,
                config_seed: Some(config_seed),
                ..Default::default()
            },
        )?;
        // Host identity is created/migrated here — `orbit init` is its sole
        // owner (ADR-0227). This runs after the root exists so the file has a
        // parent directory.
        ensure_host_identity_for_init(
            root_override,
            self.non_interactive,
            self.host_name,
            self.task_prefix,
        )?;
        let paths = reported_init_paths(root_override);
        print_init_result(InitOutput {
            skills_root: paths.skills_root,
            refreshed_skill_files: result.refreshed_skill_files,
            created_skills_symlink: result.created_skills_symlink,
            config_path: paths.config_path,
            created_config: result.created_config,
            refreshed_default_activities: result.refreshed_default_activities,
            retired_default_activities: result.retired_default_activities,
            refreshed_default_jobs: result.refreshed_default_jobs,
            retired_default_jobs: result.retired_default_jobs,
            managed_asset_warnings: result.managed_asset_warnings,
            refreshed_default_executors: result.refreshed_default_executors,
            refreshed_default_policies: result.refreshed_default_policies,
        });
        Ok(())
    }
}

/// Create or migrate this machine's host identity. Host name and task prefix are only
/// consulted when the identity is absent (a fresh create); a present identity
/// is preserved unchanged and a legacy file is migrated without prompting.
fn ensure_host_identity_for_init(
    root_override: Option<&Path>,
    non_interactive: bool,
    host_name: Option<String>,
    task_prefix: Option<String>,
) -> Result<(), OrbitError> {
    let global_root = match root_override {
        Some(root) => root.to_path_buf(),
        None => global_orbit_dir()?,
    };
    let outcome = ensure_host_identity(&global_root, move || {
        let host_id = match host_name {
            Some(name) => name,
            None if non_interactive => {
                return Err(OrbitError::InvalidInput(
                    "host identity is absent; pass --host-name and --task-prefix \
                     to initialize a fresh host non-interactively"
                        .to_string(),
                ));
            }
            None => prompt_host_name()?,
        };
        let task_prefix = match task_prefix {
            Some(prefix) => validate_new_task_prefix(&prefix)?,
            None if non_interactive => {
                return Err(OrbitError::InvalidInput(
                    "host identity is absent; pass --task-prefix <PREFIX> (2-5 uppercase ASCII letters) \
                     to initialize a fresh host non-interactively"
                        .to_string(),
                ));
            }
            None => prompt_task_prefix()?,
        };
        Ok(NewHostIdentity {
            host_id,
            task_prefix,
        })
    })?;

    report_host_identity(&outcome);
    Ok(())
}

fn report_host_identity(outcome: &HostIdentityOutcome) {
    let identity = outcome.identity();
    let verb = match outcome {
        HostIdentityOutcome::Created(_) => "created",
        HostIdentityOutcome::Migrated(_) => "migrated",
        HostIdentityOutcome::Unchanged(_) => "unchanged",
    };
    println!(
        "host identity ({verb}): host_id=\"{}\", machine_id={}, task_prefix={}",
        identity.host_id, identity.machine_id, identity.task_prefix
    );
}

fn prompt_host_name() -> Result<String, OrbitError> {
    let default = os_hostname();
    let prompt = match default.as_deref() {
        Some(name) => format!("Host name [{name}]: "),
        None => "Host name: ".to_string(),
    };
    let answer = read_line(&prompt)?;
    if answer.is_empty() {
        default.ok_or_else(|| {
            OrbitError::InvalidInput(
                "no host name entered and the OS hostname is unavailable; \
                 re-run with --host-name"
                    .to_string(),
            )
        })
    } else {
        Ok(answer)
    }
}

const MAX_TASK_PREFIX_ATTEMPTS: usize = 4;

fn prompt_task_prefix() -> Result<String, OrbitError> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stderr = io::stderr();
    let mut output = stderr.lock();

    prompt_task_prefix_from(&mut reader, &mut output)
}

pub(super) fn prompt_task_prefix_from(
    reader: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<String, OrbitError> {
    for _ in 0..MAX_TASK_PREFIX_ATTEMPTS {
        let answer = read_line_from(
            "Task prefix (2-5 uppercase ASCII letters): ",
            reader,
            output,
        )?;
        match validate_new_task_prefix(&answer) {
            Ok(prefix) => return Ok(prefix),
            Err(error) => {
                writeln!(output, "{error}").map_err(|error| OrbitError::Io(error.to_string()))?;
            }
        }
    }

    Err(OrbitError::InvalidInput(format!(
        "task prefix remained invalid after {MAX_TASK_PREFIX_ATTEMPTS} attempts; pass --task-prefix or --non-interactive"
    )))
}

fn read_line(prompt: &str) -> Result<String, OrbitError> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stderr = io::stderr();
    let mut output = stderr.lock();

    read_line_from(prompt, &mut reader, &mut output)
}

fn read_line_from(
    prompt: &str,
    reader: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<String, OrbitError> {
    write!(output, "{prompt}").map_err(|error| OrbitError::Io(error.to_string()))?;
    output
        .flush()
        .map_err(|error| OrbitError::Io(error.to_string()))?;

    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .map_err(|error| OrbitError::Io(error.to_string()))?;

    if bytes_read == 0 {
        return Err(OrbitError::InvalidInput(
            "stdin closed before an interactive prompt was answered; pass --task-prefix/--host-name or --non-interactive"
                .to_string(),
        ));
    }

    Ok(line.trim().to_string())
}

fn print_init_result(output: InitOutput) {
    println!(
        "skills: root={}, refreshed={}, symlink_created={}; config: path={}, created={}; default_activities_refreshed={}, retired={}; default_jobs_refreshed={}, retired={}; default_executors_refreshed={}; default_policies_refreshed={}",
        output.skills_root,
        output.refreshed_skill_files,
        output.created_skills_symlink,
        output.config_path,
        output.created_config,
        output.refreshed_default_activities,
        output.retired_default_activities,
        output.refreshed_default_jobs,
        output.retired_default_jobs,
        output.refreshed_default_executors,
        output.refreshed_default_policies,
    );
    for warning in output.managed_asset_warnings {
        eprintln!("warning: {warning}");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitOutput {
    skills_root: &'static str,
    refreshed_skill_files: usize,
    created_skills_symlink: bool,
    config_path: &'static str,
    created_config: bool,
    refreshed_default_activities: usize,
    retired_default_activities: usize,
    refreshed_default_jobs: usize,
    retired_default_jobs: usize,
    managed_asset_warnings: Vec<String>,
    refreshed_default_executors: usize,
    refreshed_default_policies: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct ReportedInitPaths {
    skills_root: &'static str,
    config_path: &'static str,
}

fn reported_init_paths(root_override: Option<&Path>) -> ReportedInitPaths {
    if root_override.is_some_and(|path| !orbit_core::runtime::is_global_orbit_root(path)) {
        ReportedInitPaths {
            skills_root: "<custom orbit root>/skills",
            config_path: "<custom orbit root>/config.toml",
        }
    } else {
        ReportedInitPaths {
            skills_root: "~/.orbit/skills",
            config_path: "~/.orbit/config.toml",
        }
    }
}
