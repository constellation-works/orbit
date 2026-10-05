//! Shell for the plugin-backend unit tests that the keep list retains.

use std::path::Path;

use orbit_types::plugin::{PluginFsPermissions, PluginGrant, PluginPermissions};

use super::super::backend::PluginBackendSpec;
use super::super::loader::physical_with_missing_tail;
use super::support::{context, spec};

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
