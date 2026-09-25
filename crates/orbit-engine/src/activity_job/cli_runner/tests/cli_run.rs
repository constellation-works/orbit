#![allow(missing_docs)]

//! `CliRun` owns the temp directory, fake-agent launcher, audit sink, audit
//! writer, and host for one `run_cli_backend` call. Tests pass the inputs
//! they are about — provider, event list or fixture script, spec overrides,
//! prompt — and read the outcome back.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_store::Store;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::Value;
use tempfile::TempDir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, DispatchOutcome, ResolvedSandbox};
use super::super::super::sqlite_sink::V2SqliteSink;
use super::super::run_cli_backend;
use super::test_support::{RecordingSink, TestHost, test_agent_loop_spec_for, write_executable};

const SUCCESS_ENVELOPE: &str = include_str!("fixtures/success_envelope.jsonl");
const SUCCESS_ENVELOPE_RESULT_ONLY: &str =
    include_str!("fixtures/success_envelope_result_only.jsonl");
const PLAIN_STDERR: &str = include_str!("fixtures/plain_stderr.txt");
const CAPTURED_STDERR: &str = include_str!("fixtures/captured_stderr.txt");

#[cfg(target_os = "linux")]
pub(in crate::activity_job::cli_runner) const TOUCH_ORBIT_UNGRANTED: &str =
    include_str!("fixtures/touch_orbit_ungranted.sh");
#[cfg(target_os = "linux")]
pub(in crate::activity_job::cli_runner) const TOUCH_ORBIT_UNGRANTED_EXIT_0: &str =
    include_str!("fixtures/touch_orbit_ungranted_exit_0.sh");

struct SpecOverrides {
    timeout: Duration,
    model: Option<String>,
    require_response_envelope: bool,
    require_completion_envelope: bool,
}

enum SinkMode {
    Recording,
    Sqlite,
}

enum RunSink {
    Recording(Arc<RecordingSink>),
    Sqlite(Arc<V2SqliteSink>),
}

struct Prepared {
    host: TestHost,
    spec: AgentLoopSpec,
    audit: Arc<V2AuditWriter>,
    sink: RunSink,
    script: PathBuf,
}

pub(in crate::activity_job::cli_runner) struct CliRun {
    root: TempDir,
    provider: String,
    command_name: Option<String>,
    command_path: Option<PathBuf>,
    script_body: Option<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
    consume_stdin: bool,
    run_id: String,
    audit_agent: Option<String>,
    spec: SpecOverrides,
    input: Value,
    provider_config: HashMap<String, String>,
    executor_args: Vec<String>,
    sandbox: Option<ResolvedSandbox>,
    task_context: Option<Value>,
    sink_mode: SinkMode,
    prepared: Option<Prepared>,
}

pub(in crate::activity_job::cli_runner) struct CliRunOutput {
    result: Option<Result<DispatchOutcome, DispatchError>>,
    pub audit: Arc<V2AuditWriter>,
    pub script: PathBuf,
    sink: RunSink,
    _root: TempDir,
}

impl CliRunOutput {
    pub(in crate::activity_job::cli_runner) fn take_result(
        &mut self,
    ) -> Result<DispatchOutcome, DispatchError> {
        self.result.take().expect("cli run result already taken")
    }

    pub(in crate::activity_job::cli_runner) fn recording(&self) -> &RecordingSink {
        match &self.sink {
            RunSink::Recording(sink) => sink,
            RunSink::Sqlite(_) => {
                panic!("cli run was built with sqlite_blobs(), not a recording sink")
            }
        }
    }

    pub(in crate::activity_job::cli_runner) fn sqlite(&self) -> &V2SqliteSink {
        match &self.sink {
            RunSink::Sqlite(sink) => sink,
            RunSink::Recording(_) => {
                panic!("cli run was built with the recording sink, not sqlite_blobs()")
            }
        }
    }
}

impl CliRun {
    pub(in crate::activity_job::cli_runner) fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("cli run tempdir"),
            provider: "codex".to_string(),
            command_name: None,
            command_path: None,
            script_body: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
            consume_stdin: true,
            run_id: "job-test".to_string(),
            audit_agent: None,
            spec: SpecOverrides {
                timeout: Duration::from_secs(5),
                model: None,
                require_response_envelope: false,
                require_completion_envelope: true,
            },
            input: Value::Object(serde_json::Map::new()),
            provider_config: HashMap::new(),
            executor_args: Vec::new(),
            sandbox: None,
            task_context: None,
            sink_mode: SinkMode::Recording,
            prepared: None,
        }
    }

    pub(in crate::activity_job::cli_runner) fn provider(
        mut self,
        provider: impl Into<String>,
    ) -> Self {
        self.provider = provider.into();
        self
    }

    pub(in crate::activity_job::cli_runner) fn command_name(
        mut self,
        name: impl Into<String>,
    ) -> Self {
        self.command_name = Some(name.into());
        self
    }

    pub(in crate::activity_job::cli_runner) fn command_path(
        mut self,
        path: impl Into<PathBuf>,
    ) -> Self {
        self.command_path = Some(path.into());
        self
    }

    /// Install a complete fake-agent program. Used when the test's subject is
    /// the script itself (a denied write) rather than an event stream.
    pub(in crate::activity_job::cli_runner) fn script(mut self, body: impl Into<String>) -> Self {
        self.script_body = Some(body.into());
        self
    }

    /// Append stdout events. Each item is one line; a missing trailing
    /// newline is added so the bytes match `printf '%s\n'`.
    pub(in crate::activity_job::cli_runner) fn events<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.script_body = None;
        for line in lines {
            push_line(&mut self.stdout, line.as_ref());
        }
        self
    }

    pub(in crate::activity_job::cli_runner) fn stderr_events<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.script_body = None;
        for line in lines {
            push_line(&mut self.stderr, line.as_ref());
        }
        self
    }

    /// Append exact stdout bytes, including any newline the fixture already has.
    pub(in crate::activity_job::cli_runner) fn stdout_exact(
        mut self,
        bytes: impl AsRef<str>,
    ) -> Self {
        self.script_body = None;
        self.stdout.extend_from_slice(bytes.as_ref().as_bytes());
        self
    }

    pub(in crate::activity_job::cli_runner) fn stderr_exact(
        mut self,
        bytes: impl AsRef<str>,
    ) -> Self {
        self.script_body = None;
        self.stderr.extend_from_slice(bytes.as_ref().as_bytes());
        self
    }

    pub(in crate::activity_job::cli_runner) fn success_envelope(self) -> Self {
        self.stdout_exact(SUCCESS_ENVELOPE)
    }

    pub(in crate::activity_job::cli_runner) fn success_envelope_result_only(self) -> Self {
        self.stdout_exact(SUCCESS_ENVELOPE_RESULT_ONLY)
    }

    pub(in crate::activity_job::cli_runner) fn plain_stderr(self) -> Self {
        self.stderr_exact(PLAIN_STDERR)
    }

    pub(in crate::activity_job::cli_runner) fn captured_stderr(self) -> Self {
        self.stderr_exact(CAPTURED_STDERR)
    }

    pub(in crate::activity_job::cli_runner) fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }

    pub(in crate::activity_job::cli_runner) fn consume_stdin(mut self, consume: bool) -> Self {
        self.consume_stdin = consume;
        self
    }

    pub(in crate::activity_job::cli_runner) fn run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = run_id.into();
        self
    }

    pub(in crate::activity_job::cli_runner) fn audit_agent(
        mut self,
        agent: impl Into<String>,
    ) -> Self {
        self.audit_agent = Some(agent.into());
        self
    }

    pub(in crate::activity_job::cli_runner) fn timeout(mut self, timeout: Duration) -> Self {
        self.spec.timeout = timeout;
        self
    }

    pub(in crate::activity_job::cli_runner) fn model(mut self, model: impl Into<String>) -> Self {
        self.spec.model = Some(model.into());
        self
    }

    pub(in crate::activity_job::cli_runner) fn require_response_envelope(
        mut self,
        require: bool,
    ) -> Self {
        self.spec.require_response_envelope = require;
        self
    }

    pub(in crate::activity_job::cli_runner) fn require_completion_envelope(
        mut self,
        require: bool,
    ) -> Self {
        self.spec.require_completion_envelope = require;
        self
    }

    pub(in crate::activity_job::cli_runner) fn input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    pub(in crate::activity_job::cli_runner) fn provider_config(
        mut self,
        config: HashMap<String, String>,
    ) -> Self {
        self.provider_config = config;
        self
    }

    pub(in crate::activity_job::cli_runner) fn executor_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.executor_args = args.into_iter().map(Into::into).collect();
        self
    }

    pub(in crate::activity_job::cli_runner) fn sandbox(mut self, sandbox: ResolvedSandbox) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    pub(in crate::activity_job::cli_runner) fn task_context(mut self, context: Value) -> Self {
        self.task_context = Some(context);
        self
    }

    /// Store stdout, stderr, and stdin blobs in `V2SqliteSink` instead of the
    /// in-memory recording sink. The test is about that store.
    pub(in crate::activity_job::cli_runner) fn sqlite_blobs(mut self) -> Self {
        self.sink_mode = SinkMode::Sqlite;
        self
    }

    pub(in crate::activity_job::cli_runner) fn root(&self) -> &Path {
        self.root.path()
    }

    pub(in crate::activity_job::cli_runner) fn input_value(&self) -> &Value {
        &self.input
    }

    pub(in crate::activity_job::cli_runner) fn host_and_spec(
        &mut self,
    ) -> (&TestHost, &AgentLoopSpec) {
        self.ensure_prepared();
        let prepared = self.prepared.as_ref().expect("cli run prepared");
        (&prepared.host, &prepared.spec)
    }

    pub(in crate::activity_job::cli_runner) fn spec_mut(&mut self) -> &mut AgentLoopSpec {
        self.ensure_prepared();
        &mut self.prepared.as_mut().expect("cli run prepared").spec
    }

    pub(in crate::activity_job::cli_runner) fn run(mut self) -> CliRunOutput {
        self.ensure_prepared();
        let prepared = self.prepared.take().expect("cli run prepared");
        let result = run_cli_backend(
            &prepared.host,
            &prepared.spec,
            "test_activity",
            &self.run_id,
            Arc::clone(&prepared.audit),
            &self.input,
            None,
        );
        CliRunOutput {
            result: Some(result),
            audit: prepared.audit,
            script: prepared.script,
            sink: prepared.sink,
            _root: self.root,
        }
    }

    fn ensure_prepared(&mut self) {
        if self.prepared.is_some() {
            return;
        }
        let script = self.resolved_command_path();
        self.write_agent(&script);
        let host = TestHost {
            command: script.display().to_string(),
            executor_args: self.executor_args.clone(),
            provider_config: self.provider_config.clone(),
            sandbox: self.sandbox.clone(),
            task_context: self.task_context.clone(),
            workspace_root: None,
            orbit_registry_root: None,
            orbit_workspace_selector: None,
        };
        let mut spec = test_agent_loop_spec_for(&self.provider, self.spec.timeout);
        spec.model.clone_from(&self.spec.model);
        spec.require_response_envelope = self.spec.require_response_envelope;
        spec.require_completion_envelope = self.spec.require_completion_envelope;
        let audit_agent = self
            .audit_agent
            .clone()
            .unwrap_or_else(|| format!("{}:gpt-5.5", self.provider));
        let sink = match self.sink_mode {
            SinkMode::Recording => RunSink::Recording(Arc::new(RecordingSink::default())),
            SinkMode::Sqlite => {
                let sqlite = Arc::new(V2SqliteSink::new(
                    Arc::new(Store::open_in_memory().expect("open sqlite store")),
                    "ws-test",
                    self.run_id.clone(),
                    audit_agent.clone(),
                    None,
                    self.root.path().join("audit").join("blobs"),
                ));
                RunSink::Sqlite(sqlite)
            }
        };
        let sink_for_writer: Arc<dyn AuditSink> = match &sink {
            RunSink::Recording(recording) => recording.clone(),
            RunSink::Sqlite(sqlite) => sqlite.clone(),
        };
        let audit = Arc::new(V2AuditWriter::new(
            self.run_id.clone(),
            audit_agent,
            sink_for_writer,
        ));
        self.prepared = Some(Prepared {
            host,
            spec,
            audit,
            sink,
            script,
        });
    }

    fn resolved_command_path(&self) -> PathBuf {
        if let Some(path) = &self.command_path {
            return path.clone();
        }
        let name = self
            .command_name
            .as_deref()
            .unwrap_or(self.provider.as_str());
        self.root.path().join(name)
    }

    fn write_agent(&self, path: &Path) {
        if let Some(body) = &self.script_body {
            write_executable(path, body);
            return;
        }
        let dir = path.parent().expect("agent script parent");
        let stem = path
            .file_name()
            .expect("agent script name")
            .to_string_lossy();
        let stdout_path = dir.join(format!(".{stem}.stdout"));
        let stderr_path = dir.join(format!(".{stem}.stderr"));
        fs::write(&stdout_path, &self.stdout).expect("write fake-agent stdout");
        fs::write(&stderr_path, &self.stderr).expect("write fake-agent stderr");
        let consume = if self.consume_stdin {
            "cat > /dev/null\n"
        } else {
            ""
        };
        let body = format!(
            "#!/bin/sh\n{consume}cat {stdout}\ncat {stderr} >&2\nexit {code}\n",
            stdout = shell_quote(&stdout_path),
            stderr = shell_quote(&stderr_path),
            code = self.exit_code,
        );
        write_executable(path, &body);
    }
}

fn push_line(dest: &mut Vec<u8>, line: &str) {
    dest.extend_from_slice(line.as_bytes());
    if !line.ends_with('\n') {
        dest.push(b'\n');
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
