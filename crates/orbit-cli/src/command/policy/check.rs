use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::policy::DEFAULT_POLICY_NAME;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

use super::support::status_word;

#[derive(Args)]
pub struct PolicyCheckArgs {
    pub profile_name: String,
    pub path: String,
    #[arg(long)]
    pub json: bool,
}

impl Execute for PolicyCheckArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let def = runtime
            .get_policy_def(DEFAULT_POLICY_NAME)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("policy not found: {}", DEFAULT_POLICY_NAME))
            })?;

        let read = def.check_path(
            &self.profile_name,
            orbit_types::policy::FsOperation::Read,
            &self.path,
        )?;
        let modify = def.check_path(
            &self.profile_name,
            orbit_types::policy::FsOperation::Modify,
            &self.path,
        )?;

        let doc = json!({
            "policy": DEFAULT_POLICY_NAME,
            "profile": self.profile_name,
            "path": self.path,
            "read": {
                "allowed": read.allowed,
                "matched_rule": read.matched_rule,
            },
            "modify": {
                "allowed": modify.allowed,
                "matched_rule": modify.matched_rule,
            },
        });
        let text = format!(
            "Policy:  {}\nProfile: {}\nPath:    {}\nread:    {} ({})\nmodify:  {} ({})",
            DEFAULT_POLICY_NAME,
            self.profile_name,
            self.path,
            status_word(read.allowed),
            read.matched_rule,
            status_word(modify.allowed),
            modify.matched_rule
        );
        Ok(Payload::detail(doc, text).into())
    }
}
