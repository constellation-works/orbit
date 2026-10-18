//! Conflicts on generated files are regenerated, never merged [ORB-16784].
//!
//! `workflow.generated_paths` maps a generated path glob (`docs/INDEX.md`)
//! to the command that rebuilds it (`make docs-index`). A text merge of
//! generated output is meaningless: the right content is whatever the
//! generator writes from the resolved sources. So when a stopped rebase
//! conflicts on a configured path, the conflict recovery leaf leaves it alone
//! and the host continuation, after the leaf has repaired every other file,
//! takes the upstream side (the other side when upstream deleted it), runs
//! the command in the checkout and stages its output with the rest of the
//! resolution. A command that fails, times out or leaves conflict markers
//! fails the repair.

use std::fs;
use std::path::Path;

use orbit_common::fs::path_glob::RelativePathGlob;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process_supervised};

use crate::context::{GeneratedPath, RuntimeHost};

use super::rebase_recovery::git_mutation_output;
use super::rebase_repairs::has_conflict_markers;

/// How long one regenerate command may run.
const REGENERATE_TIMEOUT_MS: u64 = 10 * 60 * 1000;

/// The conflicting paths that match a configured generated path, each with
/// the command that regenerates it. The first matching glob wins.
pub(crate) fn conflicted_generated_paths(
    host: &dyn RuntimeHost,
    conflicting_paths: &[String],
) -> Vec<GeneratedPath> {
    let configured = host
        .generated_paths()
        .into_iter()
        .filter_map(|generated| {
            RelativePathGlob::new(&generated.path)
                .ok()
                .map(|glob| (glob, generated.command))
        })
        .collect::<Vec<_>>();
    conflicting_paths
        .iter()
        .filter_map(|path| {
            configured
                .iter()
                .find(|(glob, _)| glob.matches(path))
                .map(|(_, command)| GeneratedPath {
                    path: path.clone(),
                    command: command.clone(),
                })
        })
        .collect()
}

/// Take a side of each conflicted generated path in `root`, then run each
/// distinct regenerate command once. The index is left untouched: the
/// continuation stages the regenerated files with the rest of the
/// resolution. The error is the reason the repair fails.
pub(super) fn regenerate(
    host: &dyn RuntimeHost,
    root: &Path,
    generated: &[GeneratedPath],
) -> Result<(), String> {
    for entry in generated {
        take_a_side(root, &entry.path)?;
    }
    let mut commands = Vec::<&str>::new();
    for entry in generated {
        if !commands.contains(&entry.command.as_str()) {
            commands.push(&entry.command);
        }
    }
    for command in commands {
        run_generator(host, root, command)?;
    }
    for entry in generated {
        match fs::read(root.join(&entry.path)) {
            Ok(content) if has_conflict_markers(&content) => {
                return Err(format!(
                    "generator `{}` left conflict markers in generated path {}",
                    entry.command, entry.path
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("read regenerated path {}: {error}", entry.path));
            }
        }
    }
    Ok(())
}

/// Write the upstream side (`--ours` while rebasing), or the replayed side
/// when upstream has none, over the conflicted file. Only the worktree is
/// written; the unmerged index entries stay for the continuation to stage.
fn take_a_side(root: &Path, path: &str) -> Result<(), String> {
    for side in ["--ours", "--theirs"] {
        let output = git_mutation_output(root, &["checkout", side, "--", path])
            .map_err(|error| format!("take a side of generated path {path}: {error}"))?;
        if output.success {
            return Ok(());
        }
    }
    // Neither side has the file (deleted on both): the generator decides.
    Ok(())
}

fn run_generator(host: &dyn RuntimeHost, root: &Path, command: &str) -> Result<(), String> {
    let supervised = run_process_supervised(
        &ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            current_dir: Some(root.to_string_lossy().into_owned()),
            timeout_ms: Some(REGENERATE_TIMEOUT_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(
                host.validation_subprocess_environment().env,
            ),
            debug: false,
        },
        &NoSandbox,
    )
    .map_err(|error| format!("generator `{command}` could not run: {error}"))?;
    if let Some(cause) = supervised.stop_cause {
        return Err(format!("generator `{command}` was stopped: {cause}"));
    }
    let outcome = supervised.result;
    if outcome.timed_out {
        return Err(format!(
            "generator `{command}` timed out after {REGENERATE_TIMEOUT_MS} ms"
        ));
    }
    if !outcome.success {
        let stderr = outcome.stderr.trim();
        let tail = stderr
            .char_indices()
            .rev()
            .nth(2000)
            .map_or(stderr, |(index, _)| &stderr[index..]);
        return Err(format!(
            "generator `{command}` failed with exit code {:?}: {tail}",
            outcome.exit_code
        ));
    }
    Ok(())
}
