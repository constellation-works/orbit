use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use orbit_exec::{
    bwrap_program_for_audit, claude_state_dir_from_env, compile_linux_bwrap_argv,
    sandbox_exec_program_for_audit,
};
use orbit_types::workflow::ExecutorSandboxKind;
use serde_json::Value;

use super::super::dispatcher::ResolvedSandbox;

/// Build the argv we audit-log. When wrapped, the parent process the kernel
/// sees is the trusted `sandbox-exec`, so we prepend
/// `<trusted sandbox-exec> -f <profile_path>` to
/// the child program. The profile path is the literal `<profile.sb>` because
/// the real path is a tempfile created at spawn time and only meaningful to
/// the kernel — the placeholder keeps the audit record stable across runs.
pub(super) fn audit_argv_for_dispatch(
    program: &str,
    args: &[String],
    sandbox: Option<&ResolvedSandbox>,
) -> Vec<String> {
    match sandbox {
        Some(sb) if sb.kind == ExecutorSandboxKind::MacosSandboxExec => {
            let mut out = Vec::with_capacity(args.len() + 4);
            out.push(sandbox_exec_program_for_audit().to_string());
            out.push("-f".to_string());
            out.push("<profile.sb>".to_string());
            out.push(program.to_string());
            out.extend(args.iter().cloned());
            out
        }
        Some(sb) if sb.kind == ExecutorSandboxKind::LinuxBwrap => {
            try_audit_argv_for_dispatch(program, args, Some(sb), None).unwrap_or_else(|_| {
                let mut out = vec![
                    bwrap_program_for_audit().to_string(),
                    "<invalid-linux-bwrap-plan>".to_string(),
                    program.to_string(),
                ];
                out.extend(args.iter().cloned());
                out
            })
        }
        _ => {
            let mut out = Vec::with_capacity(args.len() + 1);
            out.push(program.to_string());
            out.extend(args.iter().cloned());
            out
        }
    }
}

pub(super) fn try_audit_argv_for_dispatch(
    program: &str,
    args: &[String],
    sandbox: Option<&ResolvedSandbox>,
    cwd: Option<&Path>,
) -> Result<Vec<String>, orbit_common::OrbitError> {
    match sandbox {
        Some(sb) if sb.kind == ExecutorSandboxKind::LinuxBwrap => {
            let plan =
                compile_linux_bwrap_argv(&sb.fs_profile, program, args, cwd, sb.managed_worktree)?;
            let mut out = Vec::with_capacity(plan.args.len() + 1);
            out.push(plan.wrapper);
            out.extend(plan.args);
            Ok(out)
        }
        _ => Ok(audit_argv_for_dispatch(program, args, sandbox)),
    }
}

/// Apply a trusted-host `provider_sandbox` label to the provider CLI config.
///
/// Codex is the only provider whose inner sandbox is a dynamic `--sandbox`
/// flag. Other providers ignore the label here because they have no
/// configurable inner sandbox to honour.
pub(super) fn apply_trusted_host_provider_sandbox(
    provider: &str,
    input: &Value,
    provider_config: &mut HashMap<String, String>,
) {
    let Some(label) = input.get("provider_sandbox").and_then(Value::as_str) else {
        return;
    };
    let Some((_, mode)) = orbit_types::workflow::parse_provider_sandbox_label(label) else {
        return;
    };
    if provider == "codex" {
        provider_config.insert("sandbox".to_string(), mode.to_string());
    }
}

/// Pin codex's `--sandbox` to `danger-full-access`, drop gemini's `-s` /
/// `--sandbox` toggle, and drop grok's `--sandbox <profile>` value so the
/// inner CLI sandbox does not double-encode the outer orbit-exec sandbox.
/// Claude has no native sandbox flag — nothing to neutralize.
pub(super) fn neutralize_inner_sandbox(
    provider: &str,
    provider_config: &mut HashMap<String, String>,
    static_args: &mut Vec<String>,
) {
    match provider {
        "codex" => {
            provider_config.insert("sandbox".to_string(), "danger-full-access".to_string());
        }
        "gemini" | "antigravity" => {
            *static_args = filter_gemini_inner_sandbox_args(static_args);
        }
        "grok" => {
            *static_args = filter_grok_inner_sandbox_args(static_args);
        }
        _ => {}
    }
}

/// Sandbox-orthogonal arg fixups the dispatcher applies before spawn. Today
/// this only normalizes Claude's `--debug-file` path so the log lands at a
/// sandbox-allowed location regardless of how the executor YAML spelled it.
pub(super) fn apply_provider_static_arg_fixups(provider: &str, static_args: &mut [String]) {
    if provider == "claude" {
        rewrite_claude_debug_file_path(static_args);
    }
}

/// Per-invocation argv fixups that depend on the remaining spawn deadline.
///
/// Today this only merges Antigravity `--print-timeout` so `agy`'s documented
/// 5m default cannot cut off a longer Orbit activity budget. [ORB-11337]
pub(super) fn apply_provider_runtime_arg_fixups(
    provider: &str,
    args: &mut Vec<String>,
    remaining_deadline: Duration,
) {
    orbit_agent::apply_antigravity_print_timeout(provider, args, remaining_deadline);
}

/// Replace the value following any `--debug-file` token in `static_args`
/// with `<claude_state_dir>/<basename>`. Falls back to leaving the args
/// untouched when the state dir is unresolvable (e.g. `HOME` and
/// `CLAUDE_CONFIG_DIR` both unset) — the original relative path still
/// works in non-sandboxed runs, and the sandbox failure mode is what the
/// caller is opting into.
fn rewrite_claude_debug_file_path(static_args: &mut [String]) {
    let Some(state_dir) = claude_state_dir_from_env() else {
        return;
    };
    rewrite_debug_file_value(static_args, &state_dir);
}

// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn rewrite_debug_file_value(static_args: &mut [String], state_dir: &std::path::Path) {
    let mut idx = 0;
    while idx + 1 < static_args.len() {
        if static_args[idx] == "--debug-file" {
            let basename = std::path::Path::new(&static_args[idx + 1])
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "claude-debug.log".to_string());
            static_args[idx + 1] = state_dir.join(basename).display().to_string();
            idx += 2;
        } else {
            idx += 1;
        }
    }
}

/// Strip gemini's sandbox flags from a static-args vector. `-s` and
/// `--sandbox` are toggle flags (no value); `--sandbox-image` would take a
/// value but gemini's sandbox-image is not currently used by orbit and is
/// out of scope.
fn filter_gemini_inner_sandbox_args(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| a.as_str() != "-s" && a.as_str() != "--sandbox")
        .cloned()
        .collect()
}

/// Strip grok's sandbox flag from a static-args vector. `--sandbox` takes a
/// value and may also be spelled `--sandbox=<profile>`.
fn filter_grok_inner_sandbox_args(args: &[String]) -> Vec<String> {
    let mut filtered = Vec::with_capacity(args.len());
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--sandbox" {
            idx += 2;
            continue;
        }
        if arg.starts_with("--sandbox=") {
            idx += 1;
            continue;
        }
        filtered.push(arg.clone());
        idx += 1;
    }
    filtered
}
