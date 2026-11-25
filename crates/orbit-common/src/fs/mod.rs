//! Shared filesystem, locking, path-safety, atomic I/O, Git, glob, and selector primitives; imports `error`, `process`, and `orbit-types`.

pub mod archive;
pub mod cwd;
pub mod directory_lock;
pub mod file_lock;
pub mod generation;
pub mod git;
pub mod io;
pub mod lock_mode;
pub mod lock_wait;
pub mod overlap_index;
pub mod path;
pub mod path_filter;
pub mod path_glob;
pub mod physical;
pub mod reverse_lines;
pub mod selector;

pub use io::open_read_only_no_follow;
