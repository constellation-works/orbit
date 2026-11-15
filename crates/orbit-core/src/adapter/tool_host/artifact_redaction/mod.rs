mod audit;
mod orchestration;
mod policy;
mod sanitize;

use std::collections::BTreeSet;

pub(super) use orchestration::{finish_tool_response, sanitize_tool_input};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ArtifactRedactionKind {
    Env,
    Pattern,
    HomeDir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArtifactRedactionField {
    pub field_path: String,
    pub kinds: BTreeSet<ArtifactRedactionKind>,
    pub classes: BTreeSet<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ArtifactRedactionReport {
    fields: Vec<ArtifactRedactionField>,
}
