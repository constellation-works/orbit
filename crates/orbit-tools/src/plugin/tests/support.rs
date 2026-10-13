//! Spec and tracing fixtures for the admitted private diagnostic test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::super::backend::PluginBackendSpec;
use orbit_types::plugin::{
    PluginGrant, PluginGrantSet, PluginPermissions, PluginProvenance, PluginSandbox,
};

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

/// Run `f` synchronously with this thread's `tracing` events at `INFO` and
/// above written directly to a buffer, and return the completed text beside
/// `f`'s result: the log surface a secret value must never reach.
///
/// Callers must isolate the test process when other tests can reach the same
/// callsites: scoped subscribers still share tracing's global interest cache.
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
