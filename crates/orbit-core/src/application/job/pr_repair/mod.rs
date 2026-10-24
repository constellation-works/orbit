//! Pull-request repair: in-run auto-fix admission and out-of-run adoption.

mod admission;
mod adoption;
mod ledger;

#[allow(unused_imports)]
pub(crate) use admission::*;
#[allow(unused_imports)]
pub(crate) use adoption::*;
#[allow(unused_imports)]
pub(crate) use ledger::*;
