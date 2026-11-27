use clap::Args;
use orbit_core::Core;
use orbit_core::app::doctor::{Command, Outcome, exec};
use orbit_types::policy::DEFAULT_POLICY_NAME;
use serde_json::json;

use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct FsAccessArgs {
    /// Filesystem profile name (for example `implementer`)
    pub profile: String,
    /// Path to check, matched as written against the profile's workspace-relative rules
    pub path: String,
}

/// `orbit doctor fs-access`: the active policy's read and modify verdicts for
/// one path under one fsProfile, without running anything.
pub(super) fn fs_access(core: &Core, profile: String, path: String) -> CommandOut {
    let Outcome::FsAccess { read, modify } = exec(
        core,
        Command::FsAccess {
            profile: profile.clone(),
            path: path.clone(),
        },
    )?
    else {
        unreachable!("an fs-access command answers with its verdicts")
    };

    let doc = json!({
        "policy": DEFAULT_POLICY_NAME,
        "profile": profile,
        "path": path,
        "read": {
            "allowed": read.allowed,
            "matched_rule": read.matched_rule,
        },
        "modify": {
            "allowed": modify.allowed,
            "matched_rule": modify.matched_rule,
        },
    });
    let verdict = |allowed: bool| if allowed { "allowed" } else { "denied" };
    let text = format!(
        "Policy:  {DEFAULT_POLICY_NAME}\nProfile: {profile}\nPath:    {path}\nread:    {} ({})\nmodify:  {} ({})",
        verdict(read.allowed),
        read.matched_rule,
        verdict(modify.allowed),
        modify.matched_rule
    );
    Ok(Payload::detail(doc, text).into())
}
