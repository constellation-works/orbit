//! The granted profile is what the kernel enforces: a write outside it is
//! refused, `requires.programs` is bounded by the caller's allowlist, and
//! `unsandboxed` is the only way around either.

use std::path::{Path, PathBuf};

use orbit_types::plugin::{
    PluginFsPermissions, PluginGrant, PluginGrantSet, PluginNetworkPermission, PluginPermissions,
    PluginSandbox, parse_grants,
};
use serde_json::json;

use super::super::backend::{PLUGIN_TIMEOUT_CEILING_MS, PluginBackendSpec};
use super::super::loader::physical_with_missing_tail;
use super::support::{context, require_sandbox, scoped_spec, spec, stub_backend, tool};
use crate::{Tool, ToolContext};

/// Writes `$1`-style paths handed in via the envelope input: `inside` under
/// the granted state directory, `outside` beside the plugin root.
const WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo inside > \"$ORBIT_PLUGIN_STATE/inside.txt\" 2>/dev/null; then result=inside_denied; fi\nif echo outside > \"$OUTSIDE\" 2>/dev/null; then result=\"$result,outside_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

const WORKSPACE_METADATA_WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo allowed > \"$ORBIT_WORKSPACE_ROOT/output/allowed.txt\" 2>/dev/null; then result=allowed_denied; fi\nif echo schedule > \"$ORBIT_WORKSPACE_ROOT/.orbit/routines/demo.yaml\" 2>/dev/null; then result=\"$result,orbit_written\"; fi\nif echo hook > \"$ORBIT_WORKSPACE_ROOT/.git/hooks/pre-commit\" 2>/dev/null; then result=\"$result,git_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

/// Writes into two subdirectories of the one write root the manifest asks
/// for. An operator who scoped the grant to the first must see the second
/// denied even though the manifest requested the tree that contains it.
const SCOPED_ROOT_WRITER_BACKEND: &str = "#!/bin/sh\ncat >/dev/null\nresult=ok\nif ! echo allowed > \"$ORBIT_WORKSPACE_ROOT/output/granted/allowed.txt\" 2>/dev/null; then result=granted_denied; fi\nif echo denied > \"$ORBIT_WORKSPACE_ROOT/output/ungranted/denied.txt\" 2>/dev/null; then result=\"$result,ungranted_written\"; fi\nprintf '{\"ok\":true,\"output\":{\"result\":\"%s\"}}\\n' \"$result\"\n";

const NOOP_BACKEND: &str =
    "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"result\":\"ok\"}}\\n'\n";

fn fs_state_permissions() -> PluginPermissions {
    PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    }
}

mod confinement;
mod grants;
mod state_isolation;
