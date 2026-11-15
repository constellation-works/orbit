//! Script step manifests and operator grants (script steps phase 1).
//!
//! A job step names a script as `target: script:<name>`. The name resolves
//! to a manifest at `.orbit/resources/scripts/<name>.yaml` in the workspace
//! whose `entry` is the program file Orbit will run in a later phase. This
//! module holds the manifest shape, the pure validation rules that do not
//! touch the filesystem (interpreter forms, inline-code flags, argument
//! templates), the content digest a grant binds, and the host-local grant
//! record. Filesystem resolution of `entry` lives with the catalog loader
//! in `orbit-engine`; nothing here executes anything.

mod execution;
mod grant;
mod grant_request;
mod manifest;
mod outputs;
mod validation;

pub use execution::*;
pub use grant::*;
pub use grant_request::*;
pub use manifest::*;
pub use outputs::*;
pub use validation::*;
