//! Pull-request repair: in-run auto-fix admission and out-of-run adoption,
//! by a drain pass or for one task.

mod admission;
mod adoption;
mod ledger;
mod task_auto_fix;

pub use task_auto_fix::{TaskAutoFixError, TaskAutoFixRefusal};
