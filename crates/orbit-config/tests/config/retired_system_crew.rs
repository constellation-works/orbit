//  The retired system lane through the public loader: `workflow.system_crew`
//  still loads for one release, warns as deprecated naming its replacement,
//  and no longer defines or aliases a `system` crew; system work routes
//  through the complexity pools.

const RETIRED_SYSTEM_CREW_KEY: &str = "workflow.system_crew";

const LEGACY: &str = r#"[workflow]
default_crew = "opus"
system_crew = "haiku"
low_complexity_crews = ["haiku"]

[crews.opus]
provider = "claude"
model = "fixture-opus"

[crews.haiku]
provider = "claude"
model = "fixture-haiku"

[crews.qa]
provider = "codex"
model = "fixture-qa"
"#;

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

/// Load `body` as the workspace config, returning it with the warnings the
/// load logged.
fn load_workspace(body: &str) -> (ResolvedConfig, String) {
    let directory = tempfile::tempdir().expect("tempdir");
    let roots = ConfigRoots::new(
        directory.path().join("global"),
        directory.path().join("workspace"),
    );
    retired_system_crew_write_config(roots.workspace(), body);
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(buffer.clone()))
        .with_ansi(false)
        .finish();
    let config = tracing::subscriber::with_default(subscriber, || ResolvedConfig::load(&roots))
        .expect("config loads");
    let logs = String::from_utf8(buffer.lock().expect("capture").clone()).expect("utf-8 logs");
    (config, logs)
}

fn retired_system_crew_write_config(root: &Path, body: &str) {
    std::fs::create_dir_all(root).expect("config directory");
    std::fs::write(root.join("config.toml"), body).expect("config document");
}

fn warned_deprecated(logs: &str) -> bool {
    logs.lines()
        .any(|line| line.contains(RETIRED_SYSTEM_CREW_KEY) && line.contains("config key is deprecated"))
}

#[test]
fn system_crew_key_loads_with_a_deprecation_warning_and_aliases_nothing() {
    let (config, logs) = load_workspace(LEGACY);

    assert!(
        warned_deprecated(&logs),
        "the key warns as deprecated: {logs}"
    );
    assert!(
        logs.lines()
            .any(|line| line.contains(RETIRED_SYSTEM_CREW_KEY) && line.contains("workflow.<tier>_complexity_crews")),
        "the warning names the replacement pools: {logs}"
    );
    // Neither the configured crew nor the legacy `qa` table is mirrored onto
    // a synthesized `system` entry any more, so the name is the retired lane.
    assert!(!config.crews.contains_key(RETIRED_SYSTEM_CREW));
    assert!(names_retired_system_crew("system", &config.crews));
    assert!(!names_retired_system_crew("haiku", &config.crews));

    let error = admit_settable_config_key(RETIRED_SYSTEM_CREW_KEY).expect_err("a deprecated key is not settable");
    assert!(
        error.to_string().contains("complexity_crews"),
        "the refusal names the replacement: {error}"
    );
}

#[test]
fn built_in_registry_has_no_system_crew_and_a_configured_one_is_ordinary() {
    let (built_in, logs) = load_workspace("[workflow]\n");
    assert!(!built_in.crews.contains_key(RETIRED_SYSTEM_CREW));
    assert!(
        !warned_deprecated(&logs),
        "no warning without the key: {logs}"
    );

    let (configured, _) = load_workspace(
        "[workflow]\ndefault_crew = \"system\"\n\n[crews.system]\nprovider = \"codex\"\nmodel = \"fixture-model\"\n",
    );
    assert!(configured.crews.contains_key(RETIRED_SYSTEM_CREW));
    assert!(
        !names_retired_system_crew("system", &configured.crews),
        "a [crews.system] table is an ordinary crew, not the retired lane"
    );
}
