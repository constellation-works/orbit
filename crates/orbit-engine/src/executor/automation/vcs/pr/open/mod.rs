mod create;
mod evidence;
mod lookup;
mod superseded;
mod template;

pub(in crate::executor::automation::vcs) use create::open_or_reuse_unchecked;
pub(in crate::executor::automation) use create::pr_open;
pub(in crate::executor::automation::vcs) use superseded::close_earlier_task_pull_requests;
