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
    /// A grid the renderer lays out: width, color, header suppression, and the
    /// plain form are applied here, not by the command.
    Table(Box<Table>),
    /// Full doctor findings, shown only after the terminal table.
    DoctorFindings(Vec<orbit_cmd::WorkspaceDoctorResult>),
}

impl Block {
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

    /// A grid block.
    pub fn table(table: Table) -> Self {
        Self::Table(Box::new(table))
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
