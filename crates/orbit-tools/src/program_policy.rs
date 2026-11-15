//! Activity program allowlist policy shared by process-spawning surfaces.
//!
//! The same gate applies to `proc.spawn`, external tools and plugin-declared
//! programs, so it lives beside `tool`/`context`/`host` rather than inside
//! the `proc.spawn` builtin leaf.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::tracing;

use crate::ToolContext;

pub(crate) fn enforce_program_allowlist(
    ctx: &ToolContext,
    tool_name: &str,
    program: &str,
) -> Result<(), OrbitError> {
    if let Some(disallowed) = &ctx.proc_disallowed_programs {
        // Check the requested basename and the physical target. This also
        // catches an absolute-path spelling or symlink to a listed program.
        let requested = Path::new(program);
        let basename = requested.file_name().and_then(|name| name.to_str());
        let resolved = if requested.components().count() > 1 {
            std::fs::canonicalize(requested).ok()
        } else {
            let child_path = ctx.proc_spawn_environment.as_deref().and_then(|pairs| {
                pairs
                    .iter()
                    .find(|(name, _)| name == "PATH")
                    .map(|(_, value)| value.as_str())
            });
            let path = child_path
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os("PATH"));
            path.and_then(|path| {
                std::env::split_paths(&path)
                    .find_map(|dir| std::fs::canonicalize(dir.join(requested)).ok())
            })
        };
        let resolved_basename = resolved
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if disallowed.iter().any(|entry| {
            entry == program
                || Some(entry.as_str()) == basename
                || resolved.as_deref() == Some(Path::new(entry))
                || resolved.as_deref() == std::fs::canonicalize(entry).ok().as_deref()
                || Some(entry.as_str()) == resolved_basename
        }) {
            tracing::warn!(
                target: "orbit.policy.deny",
                tool = tool_name,
                path = program,
                profile = "proc.disallowed_programs",
                matched_rule = disallowed.join(", ").as_str(),
            );
            return Err(OrbitError::PolicyDenied(format!(
                "program '{}' is in the activity disallow list: [{}]",
                program,
                disallowed.join(", ")
            )));
        }
        return Ok(());
    }
    // Enforce program allowlist when the call sits inside an activity-scoped
    // tool context, or when a legacy unrestricted context still has a
    // non-empty list. An activity-scoped call with an empty list denies every
    // program (fail-closed).
    let restricted = ctx.proc_spawn_activity_scoped || !ctx.proc_allowed_programs.is_empty();
    if restricted && !ctx.proc_allowed_programs.iter().any(|p| p == program) {
        let matched_rule = if ctx.proc_allowed_programs.is_empty() {
            "<no allowed programs>".to_string()
        } else {
            ctx.proc_allowed_programs.join(", ")
        };
        tracing::warn!(
            target: "orbit.policy.deny",
            tool = tool_name,
            path = program,
            profile = "proc.allowed_programs",
            matched_rule = matched_rule.as_str(),
        );
        return Err(OrbitError::PolicyDenied(format!(
            "program '{}' is not in the allowed list: [{}]",
            program, matched_rule
        )));
    }

    Ok(())
}
