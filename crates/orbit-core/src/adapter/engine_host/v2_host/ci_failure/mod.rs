//! CI-failure sweep: file one CI evidence snapshot as quarantined proposed
//! tasks (`filing`), then admit piloted tasks to the backlog (`admission`).

pub(super) mod admission;
mod cancellation;
mod cluster;
mod evidence;
mod fields;
pub(super) mod filing;
mod grouping;
mod landed_repair;
mod log_signature;
mod operator_cover;
mod repair_assessment;
mod runner_os;
