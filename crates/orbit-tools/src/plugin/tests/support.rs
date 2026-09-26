//! Shared fixtures for the plugin backend tests: a stub `exec` backend, a
//! spec with chosen grants, and the availability assertion for tests that
//! exercise a live sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;

use orbit_types::plugin::{
    PluginExecutionKind, PluginGrant, PluginGrantSet, PluginPermissions, PluginProvenance,
    PluginSandbox,
};
use serde_json::Value;

use super::super::backend::{
    DeliveredPluginSecret, PluginBackendSpec, PluginSecretRotation, PluginSecretSource,
};
use super::super::schema::CompiledSchema;
use super::super::tool::{PluginBackend, PluginTool, PluginToolBinding};
use crate::ToolContext;

pub(super) fn stub_backend(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("backend.sh");
    std::fs::write(&path, script).expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

pub(super) fn provenance(grants: &[PluginGrant]) -> PluginProvenance {
    PluginProvenance {
        name: "demo".into(),
        version: "1.0.0".into(),
        manifest_digest: "abc".into(),
        grants: grants
            .iter()
            .map(|grant| grant.as_str().to_string())
            .collect(),
    }
}

pub(super) fn spec(
    command: PathBuf,
    root: &Path,
    permissions: PluginPermissions,
    grants: &[PluginGrant],
) -> Arc<PluginBackendSpec> {
    scoped_spec(
        command,
        root,
        permissions,
        PluginGrantSet::from_grants(grants.iter().copied()),
    )
}

/// The same spec from an already-built grant set, for tests that need `fs`
/// scoped to particular roots rather than granted as the whole request.
pub(super) fn scoped_spec(
    command: PathBuf,
    root: &Path,
    permissions: PluginPermissions,
    grants: PluginGrantSet,
) -> Arc<PluginBackendSpec> {
    let global_root = root.join("global");
    Arc::new(PluginBackendSpec {
        provenance: PluginProvenance {
            name: "demo".into(),
            version: "1.0.0".into(),
            manifest_digest: "abc".into(),
            grants: grants.to_recorded(),
        },
        plugin_root: root.to_path_buf(),
        state_dir: root.join("state"),
        global_root,
        command,
        args: vec!["--serve".into()],
        timeout_ms: Some(5_000),
        sandbox: PluginSandbox::Default,
        permissions,
        programs: Vec::new(),
        program_paths: Default::default(),
        config: Default::default(),
        grants,
        secrets: Default::default(),
    })
}

pub(super) fn tool(spec: Arc<PluginBackendSpec>, output_schema: Option<Value>) -> PluginTool {
    PluginTool {
        name: "demo.hello".into(),
        verb: "hello".into(),
        description: "demo".into(),
        parameters: vec![],
        input_schema: None,
        execution_kind: PluginExecutionKind::ReadOnly,
        output_schema: output_schema
            .map(|schema| CompiledSchema::compile(schema).expect("compile output_schema")),
        binding: Arc::new(PluginToolBinding {
            provenance: spec.provenance.clone(),
            execution_kind: PluginExecutionKind::ReadOnly,
            diagnostic: None,
        }),
        backend: PluginBackend::Exec(spec),
    }
}

pub(super) fn context(cwd: &Path) -> ToolContext {
    ToolContext {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..ToolContext::default()
    }
}

/// Live-sandbox tests are `#[ignore]` in the portable suite and selected by
/// the Linux CI sandbox gate. Once selected, an unavailable sandbox is a test
/// failure rather than a green early return.
pub(super) fn require_sandbox() {
    #[cfg(target_os = "linux")]
    {
        let probe = orbit_exec::probe_landlock();
        assert!(
            probe.available,
            "plugin sandbox unavailable: {}",
            probe.detail
        );
    }
    #[cfg(target_os = "macos")]
    {
        assert!(
            orbit_exec::sandbox_exec_available(),
            "plugin sandbox unavailable: {}",
            orbit_exec::sandbox_exec_unavailable_message()
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        panic!("plugin sandbox unavailable on {}", std::env::consts::OS);
    }
}

/// An in-memory secret store with the host store's compare-and-swap: a check
/// and write under one lock, and a fresh version on every applied write.
#[derive(Default)]
pub(super) struct CasSource {
    pub(super) values: Mutex<BTreeMap<String, DeliveredPluginSecret>>,
    writes: AtomicUsize,
}

impl CasSource {
    pub(super) fn holding(values: &[(&str, &str, &str)]) -> Arc<Self> {
        let source = Self::default();
        {
            let mut stored = source.values.lock().expect("values");
            for (name, value, version) in values {
                stored.insert(
                    (*name).to_string(),
                    DeliveredPluginSecret {
                        value: (*value).to_string(),
                        version: (*version).to_string(),
                    },
                );
            }
        }
        Arc::new(source)
    }

    pub(super) fn stored(&self, name: &str) -> Option<(String, String)> {
        self.values
            .lock()
            .expect("values")
            .get(name)
            .map(|secret| (secret.value.clone(), secret.version.clone()))
    }
}

impl PluginSecretSource for CasSource {
    fn read(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, DeliveredPluginSecret>, OrbitError> {
        let values = self.values.lock().expect("values");
        Ok(names
            .iter()
            .filter_map(|name| {
                values
                    .get(name)
                    .map(|secret| (name.clone(), secret.clone()))
            })
            .collect())
    }

    fn compare_and_swap(
        &self,
        name: &str,
        value: &str,
        expected_version: Option<&str>,
    ) -> Result<PluginSecretRotation, OrbitError> {
        let mut values = self.values.lock().expect("values");
        if values.get(name).map(|secret| secret.version.as_str()) != expected_version {
            return Ok(PluginSecretRotation::Stale);
        }
        let write = self.writes.fetch_add(1, Ordering::SeqCst);
        let version = format!("rotated-{write}");
        values.insert(
            name.to_string(),
            DeliveredPluginSecret {
                value: value.to_string(),
                version: version.clone(),
            },
        );
        Ok(PluginSecretRotation::Applied { version })
    }
}

/// Run `f` with every `tracing` event at `INFO` and above written to a
/// buffer, and return that text beside `f`'s result: the log surface a
/// secret value must never reach.
pub(super) fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
    use std::io::{self, Write};
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(Arc::clone(&buffer)))
        .with_max_level(LevelFilter::INFO)
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs = String::from_utf8(buffer.lock().expect("capture").clone()).expect("utf8 logs");
    (result, logs)
}
