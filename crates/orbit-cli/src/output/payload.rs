//! What a command hands back: a machine-readable document plus the human view
//! of the same records.
//!
//! A command body builds one of these and returns it. It does not choose a
//! format, does not ask whether stdout is a terminal, and does not write to
//! stdout — `output::render` projects the payload into the mode the sink
//! resolved, per `docs/design/terminal-interface/specs/output-modes.md` §3
//! (ADR-0306, step 3).
//!
//! The invariant the two halves owe each other (spec §3): the human view may
//! omit fields and reformat values, but it may not show a value the document
//! lacks, and it may not omit a record the document includes. Building both
//! from the same collected records in one place is what makes that checkable —
//! the previous shape, an `if self.json { … } else { … }` fork in every
//! command, is what let them drift.

use serde_json::Value;

use crate::output::sink::OutputSink;
use crate::output::table::Table;

/// The result of a command: either a payload to render, or nothing.
pub enum CommandOutput {
    /// The command's effect was its output. Mutations, confirmations, and
    /// commands whose entire report is human prose on stderr return this;
    /// there is no record stream for `--format json` to project.
    Silent,
    /// Records to render in the resolved mode.
    Payload(Payload),
}

// Hand-written because a view holds a `comfy_table`-backed `Table` and a
// boxed stream closure, neither of which is `Debug`. Tests assert on command
// results with `unwrap()`/`assert!(matches!(..))`, so the shape is what has to
// be printable, not the rendering internals.
impl std::fmt::Debug for CommandOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Silent => f.write_str("Silent"),
            Self::Payload(payload) => f.debug_tuple("Payload").field(payload).finish(),
        }
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Payload")
            .field("doc", &self.doc)
            .field("view", &self.view)
            .field("records", &self.records)
            .finish()
    }
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocks(blocks) => f.debug_tuple("Blocks").field(blocks).finish(),
            Self::Document => f.write_str("Document"),
            Self::Stream(_) => f.write_str("Stream"),
        }
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Self::ConfigShow(text) => f.debug_tuple("ConfigShow").field(text).finish(),
            Self::Prose(text) => f.debug_tuple("Prose").field(text).finish(),
            Self::Notice(text) => f.debug_tuple("Notice").field(text).finish(),
            Self::Attention(rows) => f.debug_tuple("Attention").field(rows).finish(),
            Self::Table(_) => f.write_str("Table"),
            Self::DoctorFindings(rows) => f.debug_tuple("DoctorFindings").field(rows).finish(),
        }
    }
}

impl From<Payload> for CommandOutput {
    fn from(payload: Payload) -> Self {
        Self::Payload(payload)
    }
}

impl CommandOutput {
    /// The process exit code to use after this output has been rendered.
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Self::Silent => 0,
            Self::Payload(payload) => payload.exit_code(),
        }
    }

    /// The message to record on the audit row of a nonzero exit, if the
    /// command supplied one.
    pub(crate) fn audit_message(&self) -> Option<&str> {
        match self {
            Self::Silent => None,
            Self::Payload(payload) => payload.audit_message(),
        }
    }
}

/// A document and its human rendering.
pub struct Payload {
    /// The `json`-mode document: an array for a list, an object for a detail
    /// view. Also the source of `ndjson` records.
    doc: Value,
    /// How `table` and plain mode render the same records.
    view: View,
    /// The `ndjson` records of a list whose document is an object rather
    /// than the bare array; `None` derives them from `doc`.
    records: Option<Vec<Value>>,
    /// The process exit code to use after the payload has been rendered.
    exit_code: i32,
    /// Why a nonzero exit is recorded as it is in the audit row; `None` leaves
    /// the row without a message.
    audit_message: Option<String>,
}

/// One piece of a human view. A detail command is usually prose with a grid
/// or two inside it (`orbit tool show` labels a tool, then tabulates its
/// parameters), so a view is a sequence rather than one or the other.
pub enum Block {
    /// Text the command formatted itself. Written verbatim; any color in it
    /// was already gated by the sink when the command built it.
    Text(String),
    /// `orbit config show` text: aligned label/value/state/description rows
    /// and section headings the renderer fits to the terminal width in table
    /// mode (descriptions wrap aligned to their column). Plain and
    /// machine-readable modes keep the text whole, so no information is lost
    /// outside terminal fitting.
    ConfigShow(ConfigShow),
    /// Human prose, word-wrapped to the sink width in terminal table mode.
    /// Explicit line breaks survive; pipes and plain mode keep the text whole.
    Prose(String),
    /// A non-record notice — health prose, a status or count trailer — that
    /// is not part of the record stream. The renderer writes it to stderr in
    /// every mode (`output-modes.md` §5), so a pipe receives only records
    /// while a human still sees the notice.
    Notice(String),
    /// Operator attention rows, wrapped as associated per-item details on a
    /// terminal and rendered as one escaped tab-separated record per item in
    /// plain mode.
    Attention(Vec<AttentionRow>),
    /// A grid the renderer lays out: width, color, header suppression, and the
    /// plain form are applied here, not by the command.
    Table(Box<Table>),
    /// Full doctor findings, shown only after the terminal table.
    DoctorFindings(Vec<orbit_cmd::WorkspaceDoctorResult>),
}

/// The values used to render one operator attention item in human output.
#[derive(Debug)]
pub struct AttentionRow {
    pub(crate) severity: String,
    pub(crate) kind: String,
    pub(crate) host: Option<String>,
    pub(crate) workspace: String,
    pub(crate) reference: String,
    pub(crate) age: String,
    pub(crate) reason: String,
    pub(crate) next_command: String,
}

impl AttentionRow {
    /// Format a labeled item block for terminal display.
    pub(crate) fn attention_terminal_text(&self) -> String {
        let mut heading = format!("{} {}", self.severity, self.kind);
        if let Some(host) = self.host.as_ref().filter(|host| !host.is_empty()) {
            heading.push_str(&format!(" on {host}"));
        }
        if !self.workspace.is_empty() {
            heading.push_str(&format!(" — {}", self.workspace));
        }
        if !self.reference.is_empty() {
            heading.push_str(&format!(" ({})", self.reference));
        }
        if !self.age.is_empty() {
            heading.push_str(&format!(" [{}]", self.age));
        }

        let mut text = format!("{heading}\n  Reason: {}", self.reason);
        if !self.next_command.is_empty() {
            text.push_str(&format!("\n  Next: {}", self.next_command));
        }
        text
    }

    /// Fields for the stable plain record form, omitting an absent next action.
    pub(crate) fn attention_plain_fields(&self) -> Vec<&str> {
        let mut fields = vec![
            self.severity.as_str(),
            self.kind.as_str(),
            self.workspace.as_str(),
            self.reference.as_str(),
            self.age.as_str(),
            self.reason.as_str(),
        ];
        if let Some(host) = self.host.as_deref() {
            fields.insert(2, host);
        }
        if !self.next_command.is_empty() {
            fields.push(self.next_command.as_str());
        }
        fields
    }

    /// The tab-separated plain-form header for `rows`: one label per field
    /// that [`Self::attention_plain_fields`] emits, so the global `--header`
    /// labels attention the way it labels every other plain table.
    pub(crate) fn attention_plain_header(rows: &[AttentionRow]) -> String {
        let mut labels = vec!["SEVERITY", "KIND"];
        if rows.iter().any(|row| row.host.is_some()) {
            labels.push("HOST");
        }
        labels.extend(["WORKSPACE", "REF", "AGE", "REASON"]);
        if rows.iter().any(|row| !row.next_command.is_empty()) {
            labels.push("NEXT");
        }
        labels.join("\t")
    }
}

impl Block {
    /// A summary or explanation that may wrap between words on a terminal.
    pub(crate) fn prose(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.ends_with('\n') {
            text.pop();
        }
        Self::Prose(text)
    }

    /// A `config show` block. Like [`Block::text`], one trailing newline is
    /// dropped because the renderer adds its own when it prints the block.
    pub fn config_show(mut text: ConfigShow) -> Self {
        if text.text.ends_with('\n') {
            text.text.pop();
        }
        Self::ConfigShow(text)
    }

    /// A text block, for the common single-`String` case.
    ///
    /// One trailing newline is dropped: a block built with `writeln!` ends with
    /// one, and the renderer adds its own when it prints the block. Only one,
    /// so a deliberate blank line at the end of a view survives.
    pub fn text(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.ends_with('\n') {
            text.pop();
        }
        Self::Text(text)
    }

    /// A stderr notice block, for the common single-`String` case. Like
    /// [`Block::text`], one trailing newline is dropped because the renderer
    /// adds its own when it prints the block.
    pub fn notice(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.ends_with('\n') {
            text.pop();
        }
        Self::Notice(text)
    }

    /// A grid block.
    pub fn table(table: Table) -> Self {
        Self::Table(Box::new(table))
    }
}

/// Aligned configuration text with trailing-prose boundaries supplied by its producer.
/// Offsets refer to UTF-8 line starts; columns count characters, as the formatter does.
#[derive(Debug, Default)]
pub struct ConfigShow {
    pub(crate) text: String,
    pub(crate) trailing_columns: std::collections::BTreeMap<usize, usize>,
}

impl ConfigShow {
    /// Append verbatim text without changing previously recorded boundaries.
    pub(crate) fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Mark the current position as this line's trailing prose column.
    /// Call after writing all padded cells and their final gutter.
    pub(crate) fn mark_trailing_column(&mut self) {
        let line_start = self.text.rfind('\n').map_or(0, |index| index + 1);
        let column = self.text[line_start..].chars().count();
        self.trailing_columns.insert(line_start, column);
    }
}

impl std::fmt::Write for ConfigShow {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push_str(text);
        Ok(())
    }
}

/// The human rendering of a payload.
pub enum View {
    /// Prose and grids, in the order the command laid them out.
    Blocks(Vec<Block>),
    /// No human form distinct from the document. Used where the command's
    /// output has always been JSON in every mode.
    Document,
    /// A stream the renderer drives to completion, for output that cannot be
    /// collected before it is written (`orbit log tail -f` is unbounded).
    /// The renderer supplies the sink and the locked stdout handle.
    Stream(StreamFn),
}

/// A payload rendered by writing to stdout as records arrive.
pub type StreamFn =
    Box<dyn FnOnce(&OutputSink, &mut dyn std::io::Write) -> Result<(), orbit_core::OrbitError>>;

impl Payload {
    /// A list command's payload: a JSON array plus the table of the same
    /// records.
    pub fn list(records: Vec<Value>, table: Table) -> Self {
        Self {
            doc: Value::Array(records),
            view: View::Blocks(vec![Block::table(table)]),
            records: None,
            exit_code: 0,
            audit_message: None,
        }
    }

    /// A list whose document is an object carrying `records` beside
    /// list-level fields (`orbit host list`). `json` renders the object;
    /// `ndjson` writes one record per line and `table` tabulates them.
    pub fn enveloped_list(doc: Value, records: Vec<Value>, table: Table) -> Self {
        Self {
            doc,
            view: View::Blocks(vec![Block::table(table)]),
            records: Some(records),
            exit_code: 0,
            audit_message: None,
        }
    }

    /// A detail command's payload: one JSON object plus the prose the human
    /// form shows.
    pub fn detail(doc: Value, text: impl Into<String>) -> Self {
        Self::blocks(doc, vec![Block::text(text)])
    }

    /// A detail command whose human form is a fixed-shape grid rather than
    /// prose.
    pub fn detail_table(doc: Value, table: Table) -> Self {
        Self::blocks(doc, vec![Block::table(table)])
    }

    /// A detail command whose human form interleaves prose and grids.
    pub fn blocks(doc: Value, blocks: Vec<Block>) -> Self {
        Self {
            doc,
            view: View::Blocks(blocks),
            records: None,
            exit_code: 0,
            audit_message: None,
        }
    }

    /// Supply a list's NDJSON records while retaining its JSON document and
    /// human blocks, including any notices after the table.
    #[must_use]
    pub(crate) fn with_records(mut self, records: Vec<Value>) -> Self {
        self.records = Some(records);
        self
    }

    /// Set the process exit code after this payload is rendered.
    #[must_use]
    pub fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }

    /// Name the findings behind a nonzero exit so the audit row carries a
    /// message instead of a bare exit code.
    #[must_use]
    pub fn with_audit_message(mut self, message: impl Into<String>) -> Self {
        self.audit_message = Some(message.into());
        self
    }

    /// A payload with no human form of its own: rendered as its document in
    /// every mode.
    pub fn document(doc: Value) -> Self {
        Self {
            doc,
            view: View::Document,
            records: None,
            exit_code: 0,
            audit_message: None,
        }
    }

    /// A payload written as it is produced. The stream consults
    /// [`OutputSink::mode`] for line shape (`json`/`ndjson` vs human). `doc`
    /// is unused by the renderer: a stream exists because its records cannot
    /// be collected into a document before the first write.
    pub fn stream(doc: Value, stream: StreamFn) -> Self {
        Self {
            doc,
            view: View::Stream(stream),
            records: None,
            exit_code: 0,
            audit_message: None,
        }
    }

    /// The document and human rendering, for tests that inspect a payload.
    #[cfg(test)]
    pub(crate) fn into_view(self) -> (Value, View) {
        (self.doc, self.view)
    }

    /// The document, the `ndjson` records when they are not the document's
    /// own, and the human rendering, consumed by the renderer.
    pub(crate) fn into_parts(self) -> (Value, Option<Vec<Value>>, View) {
        (self.doc, self.records, self.view)
    }

    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub(crate) fn audit_message(&self) -> Option<&str> {
        self.audit_message.as_deref()
    }
}
