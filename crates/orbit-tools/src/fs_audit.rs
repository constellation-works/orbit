//! Filesystem-call audit events retained for historical harness plumbing.

use orbit_common::OrbitError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsCallEventKind {
    Request,
    Result,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsCallEvent {
    pub kind: FsCallEventKind,
    pub profile: String,
    pub op: String,
    pub path: String,
    pub allowed: bool,
    pub matched_rule: String,
}

pub trait FsAuditLogger: Send + Sync {
    fn emit(&self, event: FsCallEvent) -> Result<(), OrbitError>;
}
