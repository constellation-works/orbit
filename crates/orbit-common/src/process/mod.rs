pub mod ancestry;
pub mod bounded;
pub mod build_budget;
pub mod identity;
pub mod jitter;
pub mod output_capture;
#[cfg(unix)]
pub mod pipe;
pub mod shell;
#[cfg(unix)]
pub mod signal;
pub mod stopped_descendants;

pub use bounded::{
    BoundedRunError, CapturedOutput, run_bounded, run_bounded_capped,
    run_bounded_capped_complete_typed, run_bounded_capped_typed,
};

#[cfg(test)]
mod tests;
