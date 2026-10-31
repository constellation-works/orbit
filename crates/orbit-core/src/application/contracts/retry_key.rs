//! How a keyed pipeline submission names its retry key, shared by the
//! submitters in `task`, `review` and `job` and the admission in `job` that
//! matches it.

/// Where a keyed submission's retry key lives and how far back it is matched.
///
/// The key is the string at `input[field]`; it is matched against the job's
/// newest `scan_limit` runs in the same store transaction that would insert.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryKey<'a> {
    pub(crate) field: &'a str,
    pub(crate) scan_limit: usize,
}
