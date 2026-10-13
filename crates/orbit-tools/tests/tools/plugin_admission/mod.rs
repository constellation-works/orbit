//! Plugin admission through exported profiles and tool execution.

use std::path::Path;

use orbit_types::plugin::{PluginFsPermissions, PluginGrant, PluginPermissions};

use self::support::{context, spec};
use orbit_tools::plugin::PluginBackendSpec;
use orbit_tools::plugin::physical_with_missing_tail;

fn fs_state_permissions() -> PluginPermissions {
    PluginPermissions {
        fs: PluginFsPermissions {
            read: vec![],
            write: vec!["{{plugin_state}}".into()],
        },
        ..PluginPermissions::default()
    }
}

mod brokered;
mod confinement;
mod state_isolation;

mod support;

#[cfg(unix)]
mod rotation;
