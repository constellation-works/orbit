//! What kind of work a task is: the key its resource history is grouped
//! under, and that pull admission filters on.

use crate::workflow::AUTO_TASK_TAG_PREFIX;

use super::NO_DIFF_EXPECTED_TAG;

/// The work class of a task that reads and reports: it carries
/// `no-diff-expected`.
pub const NO_DIFF_WORK_CLASS: &str = "no-diff";
/// The work class of every other task that is not a recurring template.
pub const CHANGE_WORK_CLASS: &str = "change";

/// Whether these tags say the task was not meant to change files: they
/// carry `no-diff-expected`. The one predicate every reader of that tag
/// shares, whatever work class an `auto-task:` tag also gives the task.
pub fn is_no_diff_expected(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
}

/// The work class of a task with these tags: `auto-task:<template>` from its
/// `auto-task:` tag, else [`NO_DIFF_WORK_CLASS`] for `no-diff-expected`, else
/// [`CHANGE_WORK_CLASS`].
pub fn work_class(tags: &[String]) -> String {
    if let Some(template) = auto_task_template(tags) {
        return format!("{AUTO_TASK_TAG_PREFIX}{template}");
    }
    if is_no_diff_expected(tags) {
        return NO_DIFF_WORK_CLASS.to_string();
    }
    CHANGE_WORK_CLASS.to_string()
}

/// The template named by the first non-empty `auto-task:` tag.
pub fn auto_task_template(tags: &[String]) -> Option<&str> {
    tags.iter()
        .find_map(|tag| tag.strip_prefix(AUTO_TASK_TAG_PREFIX))
        .filter(|template| !template.is_empty())
}
