//! Admitted settings, effective `config.toml` path selection, and config accessors.

pub(crate) mod path;
pub(crate) mod settings;
mod workflow;

pub use path::{CONFIG_TOML_FILE, existing_config_file_path};
pub(crate) use path::{validated_existing_config_root, validated_runtime_config_path};
pub use settings::{HOST_SHUTDOWN_HOLD_MINUTES, OrbitRuntimeSettings};
