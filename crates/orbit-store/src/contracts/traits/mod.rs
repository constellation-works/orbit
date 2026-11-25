//! Store backend contracts, grouped by store family. Every trait is re-exported
//! here and from `contracts`, so each keeps its public path.

mod task;
mod task_records;

pub use task::*;
pub use task_records::*;
