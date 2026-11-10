//! Pull-request repair: in-run auto-fix admission and out-of-run adoption
//! (including held delivery hand-backs), by a drain pass or for one task.

mod admission;
mod adoption;
mod held;
mod ledger;
mod task_auto_fix;

pub(crate) use held::handed_back_hold;
pub use task_auto_fix::{TaskAutoFixError, TaskAutoFixRefusal};
