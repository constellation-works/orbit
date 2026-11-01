//! Independent review policy contracts [ORB-11333].
//!
//! A managed PR delivery may hold PR creation for a fresh reviewer. The
//! records here are the durable evidence that gate produces: the admission
//! snapshot a run captures, the manifest handed to the reviewer, the honest
//! verdict, the certificate that binds a passed verdict to exact base and
//! candidate trees, the mapping to the commit that actually landed, and the
//! per-lineage attempt ledger. None of these is a task status, a human
//! approval, or merge permission; they only describe what was examined.

mod admission;
mod certificate;
mod gate_note;
mod history;
mod ledger;
mod records;
mod refusal;
mod report;
mod selective;
mod verdict;
mod visual;

#[cfg(test)]
mod tests;

pub use admission::*;
pub use certificate::*;
pub use gate_note::*;
pub use history::*;
pub use ledger::*;
pub use records::*;
pub use refusal::*;
pub use report::*;
pub use selective::*;
pub use verdict::*;
pub use visual::*;
