//! The one place the CLI writes a command's records to stdout.
//!
//! `main` resolves the sink, dispatches, and hands the returned
//! [`CommandOutput`] here. This module owns the projection from payload to
//! bytes for all four modes of
//! `docs/design/terminal-interface/specs/output-modes.md` §3 (ADR-0306,
//! step 3). No command body writes a record to stdout, so a rendering
//! decision cannot be made in 154 places any more — it is made here, once,
//! from the sink `main` already resolved.
//!
//! Non-record notices follow spec §5. Ordinary notices and table trailing
//! notices go to stderr in every mode. A `TerminalNotice` whose text is
//! already in the structured document goes to stderr only in table and plain
//! modes; `json` and `ndjson` carry the text in the document without a
//! duplicate. Progress follows spec §6.

use std::io::Write;

use orbit_common::OrbitError;
use serde_json::Value;

use crate::output::common::payload::{Block, CommandOutput, View};
use crate::output::common::sink::{OutputMode, OutputSink};

/// Project a command's output into the resolved mode and write it to stdout.
pub fn emit(output: CommandOutput, sink: &OutputSink) -> Result<(), OrbitError> {
    let CommandOutput::Payload(payload) = output else {
        return Ok(());
    };
    let (doc, records, view) = payload.into_parts();

    // A stream renders itself: its records cannot be collected before the
    // first write, which is what a `json` document would require. The closure
    // reads `sink.mode()` (and `color_allowed()`) so `--format json|ndjson`
    // still selects machine-readable lines.
    if let View::Stream(stream) = view {
        let mut stdout = std::io::stdout().lock();
        return stream(sink, &mut stdout);
    }

    match sink.mode() {
        // A list's trailing notices (a truncation count, say) belong on
        // stderr in every mode (spec §5). In table and plain modes,
        // `Table::emit` prints them after the table body (ORB-12206). In
        // machine-readable modes, `Table::emit` is not called, so we print
        // them here (ORB-12203).
        OutputMode::Json => {
            for notice in table_notices(&view) {
                eprintln!("{notice}");
            }
            for notice in block_notices(&view) {
                eprintln!("{notice}");
            }
            emit_json(&doc, sink.pretty_json())
        }
        OutputMode::Ndjson => {
            for notice in table_notices(&view) {
                eprintln!("{notice}");
            }
            for notice in block_notices(&view) {
                eprintln!("{notice}");
            }
            emit_ndjson(&records.map_or(doc, Value::Array))
        }
        OutputMode::Table | OutputMode::Plain => emit_human(doc, view, sink),
    }
}

/// The trailing notices carried by a payload's table, if it has one. Read
/// here rather than inside `Table::emit` so a `json`/`ndjson` caller — which
/// never reaches `emit_human` — still receives them.
fn table_notices(view: &View) -> Vec<&str> {
    let View::Blocks(blocks) = view else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Table(table) => Some(table.trailing_notices()),
            Block::Text(_)
            | Block::Prose(_)
            | Block::Notice(_)
            | Block::TerminalNotice(_)
            | Block::Custom(_) => None,
        })
        .flatten()
        .map(String::as_str)
        .collect()
}

/// The ordinary non-record notices carried as blocks, if the view has any.
/// Read here rather than inside `emit_human` so a `json`/`ndjson` caller —
/// which never reaches `emit_human` — still receives them on stderr (spec
/// §5). `TerminalNotice` is excluded because its text is already in the
/// document and it is written to stderr only in table and plain modes.
fn block_notices(view: &View) -> Vec<&str> {
    let View::Blocks(blocks) = view else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Notice(text) => Some(text.as_str()),
            Block::Text(_)
            | Block::Prose(_)
            | Block::Table(_)
            | Block::TerminalNotice(_)
            | Block::Custom(_) => None,
        })
        .collect()
}

/// One document, pretty-printed only for a human (spec §3).
fn emit_json(doc: &Value, pretty: bool) -> Result<(), OrbitError> {
    crate::output::common::json::print_with_format(doc, pretty)
}

/// One complete JSON value per line, flushed per record (spec §3).
///
/// The flush is the point: `ndjson` is the mode a consumer picks for a long or
/// unbounded list, and a buffered writer would hold the whole list back to the
/// end and give them nothing that `json` did not already offer. A list
/// document streams its elements; a detail document is a single record.
fn emit_ndjson(doc: &Value) -> Result<(), OrbitError> {
    let mut stdout = std::io::stdout().lock();
    for record in ndjson_records(doc) {
        let line = crate::output::common::json::render(record, false)?;
        match writeln!(stdout, "{line}") {
            Ok(()) => {}
            Err(err) if crate::output::common::pipe::is_broken_pipe(&err) => return Ok(()),
            Err(err) => return Err(write_error(err)),
        }
        match stdout.flush() {
            Ok(()) => {}
            Err(err) if crate::output::common::pipe::is_broken_pipe(&err) => return Ok(()),
            Err(err) => return Err(write_error(err)),
        }
    }
    Ok(())
}

/// The records a document yields in `ndjson`: a list's elements, or the
/// document itself when it is a single detail object.
fn ndjson_records(doc: &Value) -> &[Value] {
    match doc {
        Value::Array(records) => records,
        single => std::slice::from_ref(single),
    }
}

/// The `table` and plain forms share the human view.
fn emit_human(doc: Value, view: View, sink: &OutputSink) -> Result<(), OrbitError> {
    match view {
        View::Blocks(blocks) => {
            let mut truncated = false;
            for block in blocks {
                match block {
                    Block::Text(text) => println!("{text}"),
                    Block::Prose(text) => println!("{}", sink.wrap_prose(&text)),
                    Block::Notice(text) | Block::TerminalNotice(text) => {
                        eprintln!("{}", sink.wrap_prose(&text));
                    }
                    Block::Table(table) => truncated |= table.emit(sink),
                    Block::Custom(render) => render(sink),
                }
            }
            if truncated {
                // Advisory and best effort: the table already reached stdout, so a
                // closed stderr must not turn a successful render into a panic.
                let _ = writeln!(
                    std::io::stderr(),
                    "{}",
                    sink.wrap_prose(
                        "\nTable details shortened. Use --format plain or --json for full output."
                    )
                );
            }
            Ok(())
        }
        // A payload with no human form renders as its document, pretty-printed
        // in every mode. These are the commands whose output has always been
        // JSON with no flag gating it (`tool run`, `task artifact get`), and
        // they have always been pretty: reflowing them onto one line when
        // stdout is a pipe would break the scripts already parsing them.
        // Asking for `--format json` explicitly still gets the spec's shape.
        View::Document => emit_json(&doc, true),
        // Handled before mode dispatch; a stream is mode-independent.
        View::Stream(_) => Ok(()),
    }
}

/// A failed write to stdout, other than the broken pipe `output::common::pipe`
/// already turns into a silent exit.
fn write_error(error: std::io::Error) -> OrbitError {
    OrbitError::Execution(error.to_string())
}
