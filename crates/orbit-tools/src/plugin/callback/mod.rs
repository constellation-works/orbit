//! Host-owned identity for a plugin backend that calls back into Orbit.
//!
//! The child controls its environment and its process tree: it can unset
//! every `ORBIT_*` variable the host stamped, and `setsid` gives it a pid,
//! ppid and pgid that match nothing the host recorded. Identity therefore
//! rides on something the child cannot rewrite and can only *drop*: the host
//! opens the per-call session record it wrote under
//! `{global_root}/state/plugin-callbacks/` and hands the backend that open
//! descriptor at [`PLUGIN_CALLBACK_FD`], close-on-exec cleared. Every
//! descendant inherits it across `fork` and `exec` — `setsid` does not close
//! descriptors — and a later `orbit tool run` or MCP `tools/call` reads the
//! record off the descriptor to learn which session it runs under.
//!
//! A descendant that closes the descriptor holds no credential and is
//! refused. That refusal is the sandbox's, not the process tree's: a confined
//! backend is denied the session directory (design
//! `docs/design/plugins/1_scope.md` §4.3), so a process that cannot even list
//! it is inside a plugin sandbox and a missing credential there is a refusal
//! rather than an ordinary caller.
//!
//! **A descriptor is a credential only if the host wrote what is on it.** A
//! backend can open any file it likes on the number, so three things must
//! agree: the descriptor is a regular file, it parses as a current-schema
//! record, and the record's own token names *that inode* inside the session
//! directory. The last check is what makes the credential unforgeable — a
//! plugin may write neither that directory nor any other plugin's record
//! (§4.3), so a file that answers to a name there is one the host wrote.
//!
//! The environment token plus pid/ppid/pgid ancestry that identified a
//! backend before this is kept for one release behind
//! `plugin.legacy_callback_identity`, which `orbit plugin doctor` reports for
//! as long as it is on. It is off by default: it is the path `setsid` escaped
//! [ORB-12798], and the descriptor does not have that shape.
//!
//! The record carries *authority* as well as identity: the effective tool
//! ceiling the spawning caller had when the host minted it. Knowing which
//! plugin is calling is not enough to decide a callback, because the same
//! plugin is reachable from callers with different allowlists — the
//! intersection is computed per call and the manifest list is not it
//! [ORB-12801].

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::process::ancestry::{
    current_parent_pid, current_process_group, process_start_key,
};
use orbit_types::plugin::PluginProvenance;
use serde::{Deserialize, Serialize};

use crate::upsert_env;

mod resolution;
mod session;
mod storage;

pub use self::resolution::*;
pub use self::session::*;
pub use self::storage::*;
