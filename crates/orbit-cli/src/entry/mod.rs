//! Process entry for the `orbit` binary: parse argv, classify the command,
//! admit a generation, dispatch, and report the result.

mod classify;
mod discover;
mod finish;
mod host_ssh;
mod output_args;
pub(super) mod run;

#[cfg(test)]
mod tests;
