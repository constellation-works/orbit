//! The activity tool and process policy envelope a provider subprocess
//! receives, including claimed-mode tool denial.

use orbit_common::security::child_env::{
    ACTIVITY_DEADLINE_ENV, ACTIVITY_NAME_ENV, ACTIVITY_TOOL_POLICY_ENV, ACTIVITY_TOOLS_DENY_ENV,
};
use orbit_types::policy::UNRESTRICTED_FS_PROFILE;
use orbit_types::workflow::activity_job::{ActivityToolPolicyMode, AgentLoopSpec};
use serde_json::Value;

use crate::context::{ResolvedActivityTools, RuntimeHost};

use super::super::super::dispatcher::DispatchError;

/// Legacy-allowlist entry a deny-mode run stamps when its disallow list covers
/// every registered tool. It names no tool, so an MCP server that predates
/// deny mode refuses every call instead of reading an empty
/// `ORBIT_ACTIVITY_TOOLS` as unrestricted.
const NO_CALLABLE_TOOLS_ENTRY: &str = "orbit.activity-policy.none";

/// Process-policy envelope stamped for the current activity and removed from
/// anything the outer process forwarded. [ORB-13427]
const PROC_ALLOWED_PROGRAMS_ENV: &str = "ORBIT_PROC_ALLOWED_PROGRAMS";
const PROC_PROGRAM_POLICY_ENV: &str = "ORBIT_PROC_PROGRAM_POLICY";
const PROC_DISALLOWED_PROGRAMS_ENV: &str = "ORBIT_PROC_DISALLOWED_PROGRAMS";

/// Last pre-migration shipped program lists. An older MCP server understands
/// only `ORBIT_PROC_ALLOWED_PROGRAMS`, so these preserve the old bound while
/// a new orchestrator and an old server coexist. Custom deny-mode activities
/// have no old behavior to preserve and fail closed in that window.
fn legacy_program_allowlist_for_mcp(activity: &str, disallowed: &[String]) -> Option<&'static str> {
    const SHIPPED_DISALLOWED: &[&str] = &[
        "sudo",
        "su",
        "doas",
        "pkexec",
        "ssh",
        "scp",
        "sftp",
        "rsync",
        "nc",
        "ncat",
        "netcat",
        "socat",
        "systemctl",
        "loginctl",
        "shutdown",
        "reboot",
        "mount",
        "umount",
        "chroot",
        "nsenter",
        "unshare",
        "docker",
        "podman",
    ];
    if !disallowed
        .iter()
        .map(String::as_str)
        .eq(SHIPPED_DISALLOWED.iter().copied())
    {
        return None;
    }
    match activity {
        "agent_implement" | "agent_review_repair" => Some(concat!(
            "git,make,rg,orbit,bash,sh,cat,ls,find,sed,awk,grep,jq,",
            "cargo,rustc,rustfmt,node,npm,npx,pnpm,yarn,bun,deno,",
            "python,python3,uv,poetry,pytest,ruff,mypy,go,gofmt,",
            "java,javac,mvn,gradle,dotnet,cc,c++,clang,clang++,gcc,g++,",
            "cmake,ctest,ninja,swift,ruby,bundle,rake"
        )),
        "agent_invoke" => Some("awk,bash,cargo,cat,find,git,grep,jq,ls,make,ps,python3,rg,sed,sh"),
        "pr_conflict_recovery" => Some("cargo,git,make,orbit,rg"),
        "step_failure_recovery" => Some("git,gh,orbit,rg"),
        "task_pilot" => Some("git,rg"),
        _ => None,
    }
}

/// The activity tool policy envelope for one managed agent.
///
/// Allowlist mode stamps only `ORBIT_ACTIVITY_TOOLS`, byte-for-byte what it
/// always has. Deny mode adds the policy marker, its disallow list, and the
/// activity name, and still stamps `ORBIT_ACTIVITY_TOOLS` as the concrete
/// callable set so an older MCP server enforces an equivalent allowlist.
pub fn activity_tool_policy_env(
    activity_name: &str,
    disallow_list: Option<&[String]>,
    effective_tools: &[String],
) -> Vec<(String, String)> {
    let Some(disallow_list) = disallow_list else {
        return vec![(
            "ORBIT_ACTIVITY_TOOLS".to_string(),
            effective_tools.join(","),
        )];
    };
    let allowlist = if effective_tools.is_empty() {
        NO_CALLABLE_TOOLS_ENTRY.to_string()
    } else {
        effective_tools.join(",")
    };
    vec![
        ("ORBIT_ACTIVITY_TOOLS".to_string(), allowlist),
        (
            ACTIVITY_TOOL_POLICY_ENV.to_string(),
            ActivityToolPolicyMode::Deny.as_str().to_string(),
        ),
        (ACTIVITY_TOOLS_DENY_ENV.to_string(), disallow_list.join(",")),
        (ACTIVITY_NAME_ENV.to_string(), activity_name.to_string()),
    ]
}

/// Owner task tools an agent in a claimed leaf is never granted
/// (distributed-drain design §3, "Claimed-mode implementation").
///
/// A claimed leaf's task belongs to another machine. Its injected envelope is
/// the task and the claim is the authority; `orbit.task.show` may re-read that
/// task through the claim-scoped owner broker. The agent returns its execution
/// summary in its output for `claim_handoff` to carry instead of writing owner
/// task state. Denying task updates makes that the only write path, rather
/// than relying on the prompt alone.
pub(super) const CLAIMED_MODE_DENIED_TOOLS: &[&str] = &["orbit.task.update"];

/// Whether this invocation runs inside a claimed leaf.
///
/// The trusted fact is the host's worker binding: only the process a claim is
/// bound to carries one, and it is the same for every agent step that leaf
/// runs, including the recovery hooks the executor dispatches for a failed
/// step, whose own input schema has no `claimed` field and which no pipeline
/// input reaches. `claimed: true` in the step input (the claimed pipelines
/// pass it to `agent_implement`, whose prompt also reads it) is honored too.
/// Either signal only ever removes tools, so an input that sets it outside a
/// claim narrows that run, and no input can lift the binding's guard.
pub(super) fn claimed_mode(host: &dyn RuntimeHost, input: &Value) -> bool {
    host.worker_invocation().is_some()
        || input.get("claimed").and_then(Value::as_bool) == Some(true)
}

/// The activity's deny list, extended with [`CLAIMED_MODE_DENIED_TOOLS`] in
/// claimed mode. An allowlisted activity keeps `None` here and has the same
/// tools removed from its resolved allowlist instead.
pub(super) fn claimed_tool_disallow_list(
    declared: Option<&[String]>,
    claimed: bool,
) -> Option<Vec<String>> {
    let mut list = declared?.to_vec();
    if claimed {
        for tool in CLAIMED_MODE_DENIED_TOOLS {
            if !list.iter().any(|existing| existing == tool) {
                list.push((*tool).to_string());
            }
        }
    }
    Some(list)
}

/// The tools one invocation may call, with claimed mode applied.
pub(super) struct ActivityToolGrant {
    pub(super) tool_policy: ActivityToolPolicyMode,
    pub(super) tool_disallow_list: Option<Vec<String>>,
    pub(super) activity_tools: ResolvedActivityTools,
}

/// Resolve the activity's tool grant for these tasks, removing
/// [`CLAIMED_MODE_DENIED_TOOLS`] in claimed mode.
pub(super) fn resolve_activity_tool_grant(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    activity_name: &str,
    input: &Value,
    task_ids: &[String],
) -> Result<ActivityToolGrant, DispatchError> {
    let tool_policy = spec.tool_policy_mode();
    let claimed = claimed_mode(host, input);
    let tool_disallow_list =
        claimed_tool_disallow_list(spec.tool_disallow_list.as_deref(), claimed);
    let mut activity_tools = match tool_disallow_list.as_deref() {
        Some(disallow_list) => {
            host.resolve_activity_tool_denials(task_ids, activity_name, disallow_list)?
        }
        None => host.resolve_activity_tools(task_ids, &spec.tools)?,
    };
    if claimed {
        // An allowlisted activity gets the same guard as a deny-listed one.
        for tools in [
            &mut activity_tools.requested_tools,
            &mut activity_tools.effective_tools,
        ] {
            tools.retain(|tool| !CLAIMED_MODE_DENIED_TOOLS.contains(&tool.as_str()));
        }
    }
    Ok(ActivityToolGrant {
        tool_policy,
        tool_disallow_list,
        activity_tools,
    })
}

/// This activity's tool and process policy envelope followed by its
/// filesystem profile name, in the order the child environment stamps them.
pub(super) fn activity_policy_env(
    spec: &AgentLoopSpec,
    activity_name: &str,
    tool_disallow_list: Option<&[String]>,
    effective_tools: &[String],
    fs_profile: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = activity_tool_policy_env(activity_name, tool_disallow_list, effective_tools);
    if let Some(programs) = spec.proc_allowed_programs.as_deref() {
        env.push((PROC_ALLOWED_PROGRAMS_ENV.to_string(), programs.join(",")));
    }
    if let Some(programs) = spec.proc_disallowed_programs.as_deref() {
        // An older nested MCP server ignores the deny marker and still reads
        // only this allowlist. Preserve each shipped activity's last legacy
        // bound during a mixed-version deploy; a new server uses deny mode.
        env.push((
            PROC_ALLOWED_PROGRAMS_ENV.to_string(),
            legacy_program_allowlist_for_mcp(activity_name, programs)
                .unwrap_or_default()
                .to_string(),
        ));
        env.push((PROC_PROGRAM_POLICY_ENV.to_string(), "deny".to_string()));
        env.push((PROC_DISALLOWED_PROGRAMS_ENV.to_string(), programs.join(",")));
    }
    env.push((
        "ORBIT_ACTIVITY_FS_PROFILE".to_string(),
        resolved_activity_fs_profile_name(fs_profile).to_string(),
    ));
    env
}

/// The envelope allowlist forwards an outer run's activity and process
/// policy names. An allowlist-mode run stamps no deny-mode names of its
/// own, so an inherited tool-deny marker would swap this run's tool
/// allowlist for the outer disallow list [ORB-13315], and an inherited
/// program-deny marker would make `proc.spawn` admit programs outside this
/// run's allowlist [ORB-13427]. Drop those inherited names before this
/// activity's envelope is stamped. A deny-mode activity restamps its marker,
/// its list (including an explicit empty list), and the legacy MCP allowlist.
/// An outer run's activity deadline is dropped too: every invocation stamps
/// its own.
pub(super) fn drop_inherited_policy_env(child_env: &mut Vec<(String, String)>) {
    child_env.retain(|(key, _)| {
        ![
            ACTIVITY_TOOL_POLICY_ENV,
            ACTIVITY_TOOLS_DENY_ENV,
            ACTIVITY_NAME_ENV,
            ACTIVITY_DEADLINE_ENV,
            PROC_ALLOWED_PROGRAMS_ENV,
            PROC_PROGRAM_POLICY_ENV,
            PROC_DISALLOWED_PROGRAMS_ENV,
        ]
        .contains(&key.as_str())
    });
}

fn resolved_activity_fs_profile_name(fs_profile: Option<&str>) -> &str {
    fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE)
}
