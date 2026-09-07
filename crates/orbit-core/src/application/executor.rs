use orbit_common::OrbitError;
use orbit_store::contracts::ExecutorDefStoreBackend;
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource, ResourceKind};
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

pub(crate) const DEFAULT_EXECUTOR_FILES: &[(&str, &str)] = &[
    ("claude", include_str!("../../assets/executors/claude.yaml")),
    ("codex", include_str!("../../assets/executors/codex.yaml")),
    ("gemini", include_str!("../../assets/executors/gemini.yaml")),
    (
        "antigravity",
        include_str!("../../assets/executors/antigravity.yaml"),
    ),
    ("grok", include_str!("../../assets/executors/grok.yaml")),
    (
        "copilot",
        include_str!("../../assets/executors/copilot.yaml"),
    ),
    ("cursor", include_str!("../../assets/executors/cursor.yaml")),
    ("pi", include_str!("../../assets/executors/pi.yaml")),
    (
        "opencode",
        include_str!("../../assets/executors/opencode.yaml"),
    ),
    (
        "local-shell",
        include_str!("../../assets/executors/local-shell.yaml"),
    ),
];

pub(crate) fn seed_default_executors(
    store: &dyn ExecutorDefStoreBackend,
    overwrite: bool,
) -> Result<usize, OrbitError> {
    seed_default_executors_for_platform(store, overwrite, std::env::consts::OS)
}

/// Platform-injected core of [`seed_default_executors`]. The host OS is passed
/// explicitly (rather than read from `std::env::consts::OS`) so both the macOS
/// and Linux seeding paths can be exercised deterministically in tests on a
/// single CI host. See [ORB-10112].
pub(super) fn seed_default_executors_for_platform(
    store: &dyn ExecutorDefStoreBackend,
    overwrite: bool,
    target_os: &str,
) -> Result<usize, OrbitError> {
    let mut created = 0usize;
    for (name, yaml) in DEFAULT_EXECUTOR_FILES {
        let def = parse_default_executor_for_platform(name, yaml, target_os)?;
        let existing = store.get_executor_def(&def.name)?;
        match existing {
            None => {
                store.upsert_executor_def(&def)?;
                created += 1;
            }
            Some(_) if overwrite => {
                store.upsert_executor_def(&def)?;
                created += 1;
            }
            Some(existing) => {
                if let Some(migrated) =
                    migrated_default_executor_for_platform(&existing, &def, target_os)
                {
                    store.upsert_executor_def(&migrated)?;
                    created += 1;
                }
            }
        }
    }
    Ok(created)
}

/// Select the sandbox setting a *shipped* executor should be installed with on
/// `target_os`. Shipped assets declare sandbox intent with the macOS backend;
/// Linux installs translate that marker to the native Bubblewrap backend.
///
/// This is deliberate platform selection for Orbit's own defaults, so it is
/// silent. It does NOT relax validation of user-authored executor defs: a
/// custom def that explicitly requests an incompatible sandbox backend is left
/// untouched here and still fails closed at dispatch in
/// `runtime::v2_host::sandbox`. See [ORB-10112] / [ORB-10047].
fn select_shipped_sandbox(
    declared: Option<ExecutorSandboxKind>,
    target_os: &str,
) -> Option<ExecutorSandboxKind> {
    match declared {
        Some(kind) if kind.is_available_on(target_os) => Some(kind),
        Some(_) if target_os == "linux" => Some(ExecutorSandboxKind::LinuxBwrap),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn migrated_default_executor(
    existing: &ExecutorDef,
    seeded: &ExecutorDef,
) -> Option<ExecutorDef> {
    migrated_default_executor_for_platform(existing, seeded, std::env::consts::OS)
}

pub(super) fn migrated_default_executor_for_platform(
    existing: &ExecutorDef,
    seeded: &ExecutorDef,
    target_os: &str,
) -> Option<ExecutorDef> {
    if existing.name != seeded.name {
        return None;
    }

    let mut migrated = existing.clone();
    let mut changed = false;

    if existing.executor_type == ExecutorType::AgentCli
        && seeded.executor_type == ExecutorType::DirectAgent
    {
        migrated.executor_type = ExecutorType::DirectAgent;
        changed = true;
    }

    // Re-align a leftover platform-mismatched sandbox on an installed default
    // to the platform-appropriate value chosen at seed time. Upgrading a host
    // that can't apply the persisted primitive (e.g. `macos-sandbox-exec`
    // installed on Linux before [ORB-10112]) drops it to the seeded value so
    // dispatch doesn't fail closed. See [ORB-10047].
    if let Some(kind) = existing.sandbox
        && !kind.is_available_on(target_os)
        && existing.sandbox != seeded.sandbox
    {
        tracing::debug!(
            executor = %existing.name,
            sandbox = %kind,
            target_os,
            "re-aligning platform-mismatched sandbox on installed default executor to seeded value",
        );
        migrated.sandbox = seeded.sandbox;
        changed = true;
    }

    // Linux shipped without an OS wrapper before ORB-10552, so an installed
    // default commonly has `None` rather than a mismatched concrete kind.
    // Upgrade that old shipped state to Bubblewrap on the next seed. Explicit
    // `off` is a distinct, platform-independent choice and never enters here.
    if existing.sandbox.is_none()
        && seeded.sandbox == Some(ExecutorSandboxKind::LinuxBwrap)
        && target_os == "linux"
    {
        migrated.sandbox = seeded.sandbox;
        changed = true;
    }

    if changed { Some(migrated) } else { None }
}

#[cfg(test)]
pub(super) fn parse_default_executor(name: &str, yaml: &str) -> Result<ExecutorDef, OrbitError> {
    parse_default_executor_for_platform(name, yaml, std::env::consts::OS)
}

pub(super) fn parse_default_executor_for_platform(
    name: &str,
    yaml: &str,
    target_os: &str,
) -> Result<ExecutorDef, OrbitError> {
    let resource: ExecutorResource = serde_yaml::from_str(yaml).map_err(|e| {
        OrbitError::InvalidInput(format!("invalid embedded executor def '{name}': {e}"))
    })?;
    if resource.schema_version != EXECUTOR_RESOURCE_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "invalid embedded executor def '{name}': unsupported schemaVersion {}",
            resource.schema_version
        )));
    }
    if resource.kind != ResourceKind::Executor {
        return Err(OrbitError::InvalidInput(format!(
            "invalid embedded executor def '{name}': expected kind Executor, found {}",
            resource.kind
        )));
    }
    if resource.metadata.name != name {
        return Err(OrbitError::InvalidInput(format!(
            "default executor file key '{}' does not match metadata.name '{}'",
            name, resource.metadata.name
        )));
    }

    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name,
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );

    // Shipped agent assets use their sandbox field as an opt-in marker. Keep
    // sandbox-exec on macOS, translate it to linux-bwrap on Linux, and omit it
    // on unsupported platforms. Custom definitions never pass through this
    // selector, so their explicit concrete choice remains fail-closed.
    def.sandbox = select_shipped_sandbox(def.sandbox, target_os);

    Ok(def)
}
