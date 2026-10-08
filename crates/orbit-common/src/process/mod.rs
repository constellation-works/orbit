pub mod ancestry;
pub mod bounded;
pub mod identity;
pub mod jitter;
pub mod output_capture;
pub mod shell;
pub mod stopped_descendants;

pub use bounded::{CapturedOutput, run_bounded, run_bounded_capped};

#[cfg(test)]
mod tests;
