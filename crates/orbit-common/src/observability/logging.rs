//! Tracing subscriber setup.
//!
//! One canonical initializer for any Orbit binary. Libraries should emit
//! via `tracing::{info, warn, error, debug, trace}` and never touch the
//! subscriber.
//!
//! `init_default_subscriber` writes human-readable fmt output to stderr and,
//! when possible, also appends machine-readable JSON Lines to
//! `$HOME/.orbit/state/logs/orbit.jsonl`. The JSONL feed is global rather than
//! workspace-local because logging starts before CLI argument parsing and
//! runtime root resolution.
//!
//! The JSONL file is opened on the first tracing event, not at subscriber
//! init, so commands that never log (`orbit --help`) do not create the log
//! directory or open the file. Size rotation and archive pruning (see
//! `log_rotation`; ORB-00415) run from long-lived entry points and, on first
//! write, when a single `metadata()` check shows the active file exceeds its
//! budget. Multiple Orbit processes may append to the same file at the same
//! time; readers should tolerate malformed lines because writes larger than
//! `PIPE_BUF` can interleave across processes. JSONL timestamps are assigned
//! when the formatter writes the event, which may lag event emission slightly
//! when the non-blocking writer is under load.
//!
//! Library crates enforce a `#![deny(clippy::print_stderr,
//! clippy::print_stdout)]` guard at their crate roots so new diagnostic output
//! must flow through `tracing` rather than ad-hoc stdout/stderr macros (see
//! T20260427-27). The CLI binary (`orbit-cli`) and `examples/` are exempt
//! because their stdout/stderr are user-facing surfaces.
//!
//! Redaction integration: both the stderr formatter and global JSONL formatter
//! use [`RedactingFields`], which applies [`super::redaction::redact_all`] to
//! string field values, `Error` chains, byte slices, `Debug`-formatted values,
//! and unstructured `message` text before output is written.
//! [`redact_event_text`] remains available for non-tracing surfaces that must
//! scrub text before writing it elsewhere.

use std::{
    collections::BTreeMap,
    fmt as std_fmt,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
    span,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer, Registry,
    field::{RecordFields, VisitOutput},
    fmt::{
        self, FmtContext, FormattedFields,
        format::{DefaultVisitor, FormatEvent, FormatFields, Writer},
    },
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

use crate::fs::io::{append_private_file, create_private_dir_all};
use crate::security::redaction;

static FILE_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

const ORBIT_MANAGED_RUN_CONTEXT_ENV: &str = "ORBIT_MANAGED_RUN_CONTEXT";
const ORBIT_RUN_ID_ENV: &str = "ORBIT_RUN_ID";
const ORBIT_REGISTRY_ROOT_ENV: &str = "ORBIT_REGISTRY_ROOT";

/// Field formatter that redacts string-valued and `Debug`-formatted tracing
/// event fields before they are written to stderr or the JSONL tracing feed.
///
/// Field names and typed numeric/boolean values are preserved. Span attributes
/// use the same formatter for structural compatibility, but Orbit's redaction
/// contract is intentionally scoped to event fields for now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedactingFields {
    format: RedactingFieldFormat,
}

impl RedactingFields {
    /// Human-readable field formatting for stderr.
    pub fn text() -> Self {
        Self {
            format: RedactingFieldFormat::Text,
        }
    }

    /// JSON-object field formatting for the global JSONL tracing feed.
    pub fn json() -> Self {
        Self {
            format: RedactingFieldFormat::Json,
        }
    }
}

impl Default for RedactingFields {
    fn default() -> Self {
        Self::text()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedactingFieldFormat {
    Text,
    Json,
}

impl<'writer> FormatFields<'writer> for RedactingFields {
    fn format_fields<R: RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> std_fmt::Result {
        match self.format {
            RedactingFieldFormat::Text => {
                let visitor = DefaultVisitor::new(writer, true);
                let mut visitor = RedactingVisitor::new(visitor);
                fields.record(&mut visitor);
                visitor.finish()
            }
            RedactingFieldFormat::Json => {
                let visitor = JsonFieldVisitor::new(&mut writer);
                let mut visitor = RedactingVisitor::new(visitor);
                fields.record(&mut visitor);
                visitor.finish()
            }
        }
    }

    fn add_fields(
        &self,
        current: &'writer mut FormattedFields<Self>,
        fields: &span::Record<'_>,
    ) -> std_fmt::Result {
        match self.format {
            RedactingFieldFormat::Text => {
                if !current.fields.is_empty() {
                    current.fields.push(' ');
                }
                self.format_fields(current.as_writer(), fields)
            }
            RedactingFieldFormat::Json => {
                let values = if current.fields.is_empty() {
                    BTreeMap::new()
                } else {
                    serde_json::from_str(&current.fields).map_err(|_| std_fmt::Error)?
                };
                let mut updated = String::new();
                let visitor = JsonFieldVisitor::with_values(&mut updated, values);
                let mut visitor = RedactingVisitor::new(visitor);
                fields.record(&mut visitor);
                visitor.finish()?;
                current.fields = updated;
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RedactingJsonEventFormat;

impl<S, N> FormatEvent<S, N> for RedactingJsonEventFormat
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std_fmt::Result {
        let mut fields_json = String::new();
        ctx.format_fields(Writer::new(&mut fields_json), event)?;
        let fields: Value = serde_json::from_str(&fields_json).map_err(|_| std_fmt::Error)?;
        let metadata = event.metadata();

        let mut line = serde_json::Map::new();
        line.insert(
            "timestamp".to_string(),
            Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true)),
        );
        line.insert(
            "level".to_string(),
            Value::String(metadata.level().as_str().to_string()),
        );
        line.insert("fields".to_string(), fields);
        line.insert(
            "target".to_string(),
            Value::String(metadata.target().to_string()),
        );

        let line = serde_json::to_string(&line).map_err(|_| std_fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

struct RedactingVisitor<V> {
    inner: V,
}

impl<V> RedactingVisitor<V> {
    fn new(inner: V) -> Self {
        Self { inner }
    }
}

impl<V> Visit for RedactingVisitor<V>
where
    V: Visit,
{
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.inner.record_f64(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.inner.record_i64(field, value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.inner.record_u64(field, value);
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.inner.record_i128(field, value);
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.inner.record_u128(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.inner.record_bool(field, value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let redacted = redaction::redact_all(value);
        self.inner.record_str(field, &redacted);
    }

    fn record_bytes(&mut self, field: &Field, value: &[u8]) {
        let decoded = String::from_utf8_lossy(value);
        let redacted = redaction::redact_all(decoded.as_ref());
        self.inner.record_str(field, &redacted);
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        let redacted = redaction::redact_all(&format_error_chain(value));
        self.inner.record_str(field, &redacted);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std_fmt::Debug) {
        let redacted = redaction::redact_all(&format!("{value:?}"));
        self.inner.record_debug(field, &RedactedDebug(redacted));
    }
}

fn format_error_chain(value: &(dyn std::error::Error + 'static)) -> String {
    let mut formatted = value.to_string();
    let mut source = value.source();
    while let Some(error) = source {
        formatted.push_str(": ");
        formatted.push_str(&error.to_string());
        source = error.source();
    }
    formatted
}

impl<V> VisitOutput<std_fmt::Result> for RedactingVisitor<V>
where
    V: VisitOutput<std_fmt::Result>,
{
    fn finish(self) -> std_fmt::Result {
        self.inner.finish()
    }
}

struct RedactedDebug(String);

impl std_fmt::Debug for RedactedDebug {
    fn fmt(&self, f: &mut std_fmt::Formatter<'_>) -> std_fmt::Result {
        f.write_str(&self.0)
    }
}

struct JsonFieldVisitor<'writer> {
    values: BTreeMap<String, Value>,
    writer: &'writer mut dyn std_fmt::Write,
}

impl<'writer> JsonFieldVisitor<'writer> {
    fn new(writer: &'writer mut dyn std_fmt::Write) -> Self {
        Self::with_values(writer, BTreeMap::new())
    }

    fn with_values(
        writer: &'writer mut dyn std_fmt::Write,
        values: BTreeMap<String, Value>,
    ) -> Self {
        Self { values, writer }
    }

    fn insert(&mut self, field: &Field, value: Value) {
        self.values
            .insert(json_field_name(field).to_string(), value);
    }
}

impl Visit for JsonFieldVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::String(value.to_string()));
    }

    fn record_bytes(&mut self, field: &Field, value: &[u8]) {
        self.insert(field, Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std_fmt::Debug) {
        self.insert(field, Value::String(format!("{value:?}")));
    }
}

impl VisitOutput<std_fmt::Result> for JsonFieldVisitor<'_> {
    fn finish(self) -> std_fmt::Result {
        let fields = serde_json::to_string(&self.values).map_err(|_| std_fmt::Error)?;
        self.writer.write_str(&fields)
    }
}

fn json_field_name(field: &Field) -> &str {
    field.name().strip_prefix("r#").unwrap_or(field.name())
}

/// Install the default fmt + env-filter subscriber. Safe to call multiple
/// times — subsequent calls are no-ops (mirrors the current behaviour in
/// `orbit-cli/src/main.rs`).
///
/// `default_filter` is applied when `RUST_LOG` is unset (e.g. `"warn"`,
/// `"orbit=debug"`).
///
/// The JSONL layer is installed immediately but opens the file only on the
/// first event. Path resolution reads environment variables only; it does
/// not walk the log directory or parse `config.toml`.
pub fn init_default_subscriber(default_filter: &str) {
    let filter = env_filter(default_filter);
    let stderr_layer = fmt::layer()
        .with_writer(io::stderr)
        .fmt_fields(RedactingFields::default());

    match global_jsonl_log_path() {
        Ok(path) => {
            let (file_layer, guard) = jsonl_layer_at_path(&path);
            if FILE_GUARD.set(guard).is_ok() {
                let _ = Registry::default()
                    .with(filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .try_init();
            } else {
                let _ = Registry::default()
                    .with(filter)
                    .with(stderr_layer)
                    .try_init();
                emit_log_init_warning("JSONL tracing worker guard was already initialized");
            }
        }
        Err(err) => {
            let _ = Registry::default()
                .with(filter)
                .with(stderr_layer)
                .try_init();
            emit_log_init_warning(&err.to_string());
        }
    }
}

/// Roll and prune the global JSONL feed.
///
/// Call from long-lived processes (`orbit mcp serve`, `orbit sweep` /
/// `orbit clock tick`, `orbit web serve`) so archives are reaped even when
/// the active file is within budget. Short-lived commands skip this and
/// only rotate on first JSONL write when a single `metadata()` check shows
/// the active file is oversized.
pub fn rotate_global_jsonl_best_effort() {
    let Ok(path) = global_jsonl_log_path() else {
        return;
    };
    super::log_rotation::rotate_and_prune(
        &path,
        &super::log_rotation::LogRotationConfig::load_global_best_effort(),
    );
}

/// Pre-emission scrubber for callers that need to sanitize text before writing
/// it outside the tracing pipeline. Applies env-value redaction plus the
/// default HTTP header/JSON patterns.
///
/// `init_default_subscriber` installs field-level tracing redaction
/// automatically; prefer emitting raw structured fields to `tracing::*` and
/// reserve this helper for non-tracing surfaces such as audit blobs written
/// directly to disk.
pub fn redact_event_text(message: &str) -> String {
    redaction::redact_all(message)
}

// Visible to sibling-layout logging tests; the filter construction is a
// focused seam for subscriber behavior.
pub(super) fn env_filter(default_filter: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter))
}

/// Resolve the canonical path to the global JSONL tracing feed
/// (`$HOME/.orbit/state/logs/orbit.jsonl`, or the managed child's registry
/// root). A managed child uses `ORBIT_REGISTRY_ROOT` only when the complete
/// managed-run context is present, matching global-root resolution in
/// `orbit-core`. Returned as `Result` because the path depends on environment
/// variables; callers that need a fallback should fall back to a
/// workspace-relative path or fail with a clear error.
///
/// Producers and readers MUST agree on this path — `init_default_subscriber`
/// writes here, and `orbit log tail` reads here by default.
pub fn global_jsonl_log_path() -> io::Result<PathBuf> {
    let orbit_root = match managed_registry_root()? {
        Some(root) => root,
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "cannot resolve HOME/USERPROFILE for JSONL tracing log",
                    )
                })?;
            PathBuf::from(home).join(".orbit")
        }
    };

    Ok(orbit_root.join("state").join("logs").join("orbit.jsonl"))
}

/// Resolve the registry root carried by an Orbit-managed child. The run ID is
/// part of the trust boundary: an inherited locator without it must not
/// redirect ordinary commands' logs.
fn managed_registry_root() -> io::Result<Option<PathBuf>> {
    let managed = std::env::var(ORBIT_MANAGED_RUN_CONTEXT_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    if !managed
        || std::env::var(ORBIT_RUN_ID_ENV)
            .ok()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Ok(None);
    }

    let Some(value) = std::env::var_os(ORBIT_REGISTRY_ROOT_ENV).filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let root = PathBuf::from(value);
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ORBIT_REGISTRY_ROOT must be an absolute path inside an Orbit-managed run",
        ));
    }
    Ok(Some(root))
}

// Visible to sibling-layout logging tests so file-layer behavior can be
// exercised without nesting tests under this source file.
pub(super) fn jsonl_layer_at_path<S>(
    path: &Path,
) -> (impl Layer<S> + Send + Sync + 'static + use<S>, WorkerGuard)
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let (writer, guard) = tracing_appender::non_blocking(LazyJsonlWriter::new(path.to_path_buf()));
    let layer = fmt::layer()
        .event_format(RedactingJsonEventFormat)
        .fmt_fields(RedactingFields::json())
        .with_ansi(false)
        .with_writer(writer);
    (layer, guard)
}

/// Opens the JSONL file on first write. Construction is disk-free so
/// subscriber init does not create the log directory or open the active file.
enum JsonlFile {
    Closed,
    Open(File),
    Failed,
}

struct LazyJsonlWriter {
    path: PathBuf,
    file: JsonlFile,
}

impl LazyJsonlWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: JsonlFile::Closed,
        }
    }

    fn file(&mut self) -> io::Result<&mut File> {
        match self.file {
            JsonlFile::Open(_) => {}
            JsonlFile::Failed => {
                return Err(io::Error::other("JSONL tracing log unavailable"));
            }
            JsonlFile::Closed => match open_jsonl_file(&self.path) {
                Ok(file) => self.file = JsonlFile::Open(file),
                Err(err) => {
                    let warning = err.to_string();
                    self.file = JsonlFile::Failed;
                    emit_log_init_warning(&warning);
                    return Err(err);
                }
            },
        }
        match &mut self.file {
            JsonlFile::Open(file) => Ok(file),
            JsonlFile::Failed | JsonlFile::Closed => {
                Err(io::Error::other("JSONL tracing log unavailable"))
            }
        }
    }
}

impl Write for LazyJsonlWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file()?.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            JsonlFile::Open(file) => file.flush(),
            JsonlFile::Closed | JsonlFile::Failed => Ok(()),
        }
    }
}

fn open_jsonl_file(path: &Path) -> io::Result<File> {
    super::log_rotation::rotate_if_active_exceeds_budget(
        path,
        &super::log_rotation::LogRotationConfig::load_global_best_effort(),
    );
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "cannot create JSONL tracing log directory {}: {err}",
                    parent.display()
                ),
            )
        })?;
    }
    append_private_file(path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("cannot open JSONL tracing log {}: {err}", path.display()),
        )
    })
}

// Visible to sibling-layout logging tests that verify stderr fallback behavior.
pub(super) fn emit_log_init_warning(warning: &str) {
    tracing::warn!(
        target: "orbit_common::observability::logging",
        error = warning,
        "failed to initialize JSONL tracing log"
    );
}
