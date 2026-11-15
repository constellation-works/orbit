//! The fixed fleet-notice limits: expiry defaults and caps, the size,
//! open-count and rate limits a post is held to, retention, and the bounds
//! of the brief quoted into agent prompts. Each was a `notice.*` key until
//! the keys were removed [ORB-22808]; managed agents always may post and
//! close their own notices, and opted-in activities always receive the brief.

use orbit_types::record::NoticeKind;

/// The former `notice.default_expiry_hours.incident` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const DEFAULT_EXPIRY_HOURS_INCIDENT: u32 = 72;
/// The former `notice.default_expiry_hours.maintenance` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const DEFAULT_EXPIRY_HOURS_MAINTENANCE: u32 = 12;
/// The former `notice.default_expiry_hours.info` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const DEFAULT_EXPIRY_HOURS_INFO: u32 = 168;
/// The former `notice.max_expiry_hours.operator` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const MAX_EXPIRY_HOURS_OPERATOR: u32 = 720;
/// The former `notice.max_expiry_hours.orchestrator.incident` default, fixed when the key was
/// removed [ORB-22808].
pub(crate) const MAX_EXPIRY_HOURS_ORCHESTRATOR_INCIDENT: u32 = 168;
/// The former `notice.max_expiry_hours.orchestrator.maintenance` default, fixed when the key was
/// removed [ORB-22808].
pub(crate) const MAX_EXPIRY_HOURS_ORCHESTRATOR_MAINTENANCE: u32 = 72;
/// The former `notice.max_expiry_hours.orchestrator.info` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const MAX_EXPIRY_HOURS_ORCHESTRATOR_INFO: u32 = 168;
/// The former `notice.max_expiry_hours.agent` default, fixed when the key was removed [ORB-22808].
/// It is also an agent's default expiry.
pub(crate) const MAX_EXPIRY_HOURS_AGENT: u32 = 24;
/// The former `notice.max_title_chars` default, fixed when the key was removed [ORB-22808].
pub(crate) const MAX_TITLE_CHARS: u32 = 120;
/// The former `notice.max_body_bytes` default, fixed when the key was removed [ORB-22808].
pub(crate) const MAX_BODY_BYTES: u32 = 4096;
/// The former `notice.max_evidence` default, fixed when the key was removed [ORB-22808].
pub(crate) const MAX_EVIDENCE: u32 = 8;
/// The former `notice.max_open_per_workspace` default, fixed when the key was removed [ORB-22808].
pub(crate) const MAX_OPEN_PER_WORKSPACE: u32 = 20;
/// The former `notice.max_open_global_per_owner` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const MAX_OPEN_GLOBAL_PER_OWNER: u32 = 10;
/// The former `notice.max_open_per_agent_task` default, fixed when the key was removed [ORB-22808].
pub(crate) const MAX_OPEN_PER_AGENT_TASK: u32 = 2;
/// The former `notice.agent_posts_per_task_per_day` default, fixed when the key was removed
/// [ORB-22808].
pub const AGENT_POSTS_PER_TASK_PER_DAY: u32 = 5;
/// The former `notice.orchestrator_posts_per_hour` default, fixed when the key was removed
/// [ORB-22808].
pub(crate) const ORCHESTRATOR_POSTS_PER_HOUR: u32 = 30;
/// The former `notice.retention_days` default, fixed when the key was removed [ORB-22808].
pub(crate) const RETENTION_DAYS: u32 = 30;
/// The former `notice.brief_max_items` default, fixed when the key was removed [ORB-22808].
pub(crate) const BRIEF_MAX_ITEMS: u32 = 5;
/// The former `notice.brief_body_chars` default, fixed when the key was removed [ORB-22808].
pub(crate) const BRIEF_BODY_CHARS: u32 = 600;
/// The former `notice.brief_read_timeout_ms` default, fixed when the key was removed [ORB-22808].
pub(crate) const BRIEF_READ_TIMEOUT_MS: u32 = 2000;

/// The expiry, in hours, an operator or orchestrator post of `kind` gets
/// when it names none.
pub(crate) fn default_expiry_hours(kind: NoticeKind) -> u32 {
    match kind {
        NoticeKind::Incident => DEFAULT_EXPIRY_HOURS_INCIDENT,
        NoticeKind::Maintenance => DEFAULT_EXPIRY_HOURS_MAINTENANCE,
        NoticeKind::Info => DEFAULT_EXPIRY_HOURS_INFO,
    }
}

/// The longest expiry, in hours, an orchestrator may give a notice of `kind`.
pub(crate) fn max_expiry_hours_orchestrator(kind: NoticeKind) -> u32 {
    match kind {
        NoticeKind::Incident => MAX_EXPIRY_HOURS_ORCHESTRATOR_INCIDENT,
        NoticeKind::Maintenance => MAX_EXPIRY_HOURS_ORCHESTRATOR_MAINTENANCE,
        NoticeKind::Info => MAX_EXPIRY_HOURS_ORCHESTRATOR_INFO,
    }
}
