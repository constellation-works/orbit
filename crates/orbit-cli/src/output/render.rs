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
//! Everything that is not a record goes to stderr in every mode (spec §5):
//! empty-state lines, dropped-column notices, trailing notices such as a
//! truncation count, and progress.

use std::io::Write;

use orbit_core::OrbitError;
use serde_json::Value;

use crate::output::payload::{Block, CommandOutput, View};
use crate::output::sink::{OutputMode, OutputSink};

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
            | Block::ConfigShow(_)
            | Block::Prose(_)
            | Block::Notice(_)
            | Block::DoctorFindings(_) => None,
        })
        .flatten()
        .map(String::as_str)
        .collect()
}

/// The non-record notices carried as blocks, if the view has any. Read here
/// rather than inside `emit_human` so a `json`/`ndjson` caller — which never
/// reaches `emit_human` — still receives them on stderr (spec §5).
fn block_notices(view: &View) -> Vec<&str> {
    let View::Blocks(blocks) = view else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Notice(text) => Some(text.as_str()),
            Block::Text(_)
            | Block::ConfigShow(_)
            | Block::Prose(_)
            | Block::Table(_)
            | Block::DoctorFindings(_) => None,
        })
        .collect()
}

/// One document, pretty-printed only for a human (spec §3).
fn emit_json(doc: &Value, pretty: bool) -> Result<(), OrbitError> {
    crate::output::json::print_with_format(doc, pretty)
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
        let line = crate::output::json::render(record, false)?;
        match writeln!(stdout, "{line}") {
            Ok(()) => {}
            Err(err) if crate::output::pipe::is_broken_pipe(&err) => return Ok(()),
            Err(err) => return Err(write_error(err)),
        }
        match stdout.flush() {
            Ok(()) => {}
            Err(err) if crate::output::pipe::is_broken_pipe(&err) => return Ok(()),
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
                    Block::ConfigShow(text) => {
                        if sink.mode() == OutputMode::Table {
                            println!("{}", config_show_text(&text, sink.width()));
                        } else {
                            println!("{text}");
                        }
                    }
                    Block::Prose(text) => println!("{}", sink.wrap_prose(&text)),
                    Block::Notice(text) => eprintln!("{}", sink.wrap_prose(&text)),
                    Block::Table(table) => truncated |= table.emit(sink),
                    Block::DoctorFindings(rows) => {
                        if sink.mode() == OutputMode::Table {
                            let findings = doctor_findings(&rows, sink.width());
                            if !findings.is_empty() {
                                println!("\n{findings}");
                                if truncated {
                                    println!("{}", sink.wrap_prose(
                                        "\nTable details shortened. Use --format plain or --json for full output."
                                    ));
                                }
                            }
                        }
                    }
                }
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

/// A failed write to stdout, other than the broken pipe `output::pipe`
/// already turns into a silent exit.
fn write_error(error: std::io::Error) -> OrbitError {
    OrbitError::Execution(error.to_string())
}

/// Full non-ok diagnostics in severity order. Long findings and repair
/// actions wrap onto indented lines when exceeding the terminal width.
fn doctor_findings(rows: &[orbit_cmd::WorkspaceDoctorResult], width: u16) -> String {
    use orbit_cmd::WorkspaceDoctorStatus as Status;
    let mut findings = Vec::new();
    for status in [Status::Error, Status::Warning, Status::Skipped] {
        for row in rows.iter().filter(|row| row.status == status) {
            let label = match status {
                Status::Error => "ERROR",
                Status::Warning => "WARN",
                Status::Skipped => "SKIP",
                // Informational rows are facts, not findings; the table
                // shows them.
                Status::Ok | Status::Info => continue,
            };
            let mut finding = format!(
                "{}  {label}\n{}",
                row.check_name,
                wrap_finding_message(&row.message, width)
            );
            if let Some(fix) = &row.remediation {
                finding.push('\n');
                finding.push_str(&wrap_action(fix, width));
            }
            findings.push(finding);
        }
    }
    if findings.is_empty() {
        String::new()
    } else {
        format!("Findings:\n\n{}", findings.join("\n\n"))
    }
}

/// Wrap a finding message onto indented lines, keeping each line within the terminal width.
fn wrap_finding_message(message: &str, width: u16) -> String {
    let indent = "  ";
    if width == 0 {
        return message
            .lines()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{indent}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let available = width.saturating_sub(indent.len() as u16).max(1);
    let wrapped = wrap_message(message, available);
    wrapped
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap repair action text, wrapping long actions onto indented continuation lines.
fn wrap_action(fix: &str, width: u16) -> String {
    let single_line = format!("Fix: {fix}");
    if width == 0 || single_line.chars().count() <= width as usize {
        return single_line;
    }
    let indent = "  ";
    let first_available = width.saturating_sub(5).max(1) as usize;
    let rest_available = width.saturating_sub(indent.len() as u16).max(1) as usize;

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_max = first_available;

    for word in fix.split_whitespace() {
        if current_line.is_empty() {
            current_line.push_str(word);
        } else if current_line.chars().count() + 1 + word.chars().count() <= current_max {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
            current_max = rest_available;
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }

    let mut out = String::new();
    let mut iter = lines.into_iter();
    if let Some(first) = iter.next() {
        if format!("Fix: {first}").chars().count() <= width as usize {
            out.push_str(&format!("Fix: {first}"));
        } else {
            out.push_str(&format!("Fix:\n{indent}{first}"));
        }
    }
    for line in iter {
        out.push('\n');
        out.push_str(indent);
        out.push_str(&line);
    }
    out
}

/// Use the table library's display-width-aware wrapping without the list's
/// one-line cap. No message text is truncated.
fn wrap_message(message: &str, width: u16) -> String {
    use comfy_table::{Cell, ColumnConstraint, ContentArrangement, Table, Width, presets};
    if width == 0 {
        return message.to_string();
    }
    let mut prose = Table::new();
    prose.load_style(presets::NOTHING);
    prose.force_no_tty();
    prose.set_content_arrangement(ContentArrangement::Disabled);
    prose.add_row(vec![Cell::new(message)]);
    if let Some(column) = prose.column_mut(0) {
        column.set_padding((0, 0));
        column.set_constraint(ColumnConstraint::Absolute(Width::Fixed(width)));
    }
    prose
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Row states in `config show` text (layer labels and three-way states), used
/// to locate the description column of a key row.
const CONFIG_SHOW_STATES: &[&str] = &[
    "workspace",
    "global",
    "environment",
    "built-in",
    "default",
    "unset",
    "derived",
    "identity",
    "set",
];

/// Minimum columns a description column must leave before aligned wrapping
/// gives way to a shallower hanging indent.
const CONFIG_SHOW_MIN_TRAILING: usize = 24;

/// Fit `config show` text to `width` columns. Lines that fit are left exactly
/// as written, so aligned columns survive. A longer line wraps its trailing
/// prose at word boundaries with continuations aligned to that prose's
/// column: the blurb column of a heading (after the padded title), the
/// description column of a key row (after label/value/state), the trailing
/// column of a crew row, or the value column of a Paths row. When that column
/// would leave too little room — narrow terminals where the aligned columns
/// themselves approach the width — the whole line wraps with a shallow
/// hanging indent instead, so no line exceeds the width. Width `0` (no
/// terminal width) leaves the text whole.
fn config_show_text(text: &str, width: u16) -> String {
    if width == 0 {
        return text.to_string();
    }
    let width = usize::from(width);
    let mut in_crews = false;
    let mut in_paths = false;
    text.split('\n')
        .map(|line| {
            if !line.starts_with(' ') && !line.is_empty() {
                in_crews = line.starts_with("Crews (");
                in_paths = line == "Paths";
            }
            fit_config_show_line(line, width, in_crews, in_paths)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fit_config_show_line(line: &str, width: usize, in_crews: bool, in_paths: bool) -> String {
    if line.chars().count() <= width {
        return line.to_string();
    }
    // (prefix kept on the first line, trailing prose to wrap, its column)
    let aligned = if line.starts_with("  ") {
        let tokens = token_offsets(line);
        if in_paths {
            tokens.get(1).map(|(start, _)| {
                (
                    line[..byte_at(line, *start)].to_string(),
                    line[byte_at(line, *start)..].to_string(),
                    *start,
                )
            })
        } else if in_crews {
            tokens.get(7).map(|(start, _)| {
                (
                    line[..byte_at(line, *start)].to_string(),
                    line[byte_at(line, *start)..].to_string(),
                    *start,
                )
            })
        } else if tokens.len() >= 4 && CONFIG_SHOW_STATES.contains(&tokens[2].1) {
            let start = tokens[3].0;
            Some((
                line[..byte_at(line, start)].to_string(),
                line[byte_at(line, start)..].to_string(),
                start,
            ))
        } else {
            None
        }
    } else {
        // Heading/banner: the blurb starts after the padded title's run of
        // two or more spaces.
        find_blurb_start(line).map(|start| {
            (
                line[..byte_at(line, start)].to_string(),
                line[byte_at(line, start)..].to_string(),
                start,
            )
        })
    };
    if let Some((prefix, trailing, column)) = aligned
        && prefix.chars().count() + 8 <= width
    {
        // Keep the row's columns on the first line. Continuations align to
        // the trailing column when it leaves room; on a narrow terminal
        // they fall back to a shallow indent so the prose gets full width.
        let indent = if column + CONFIG_SHOW_MIN_TRAILING <= width {
            column
        } else {
            4.min(width.saturating_sub(1))
        };
        return wrap_trailing(&prefix, &trailing, indent, width);
    }
    // Fallback: wrap the whole line with a hanging indent two columns deeper
    // than its own indent (column 0 for headings).
    let lead = line.chars().take_while(|c| *c == ' ').count();
    let indent = if lead > 0 { lead + 2 } else { 0 };
    let indent = if indent + 1 >= width {
        lead.min(width.saturating_sub(1))
    } else {
        indent
    };
    let lead = lead.min(width.saturating_sub(1));
    wrap_trailing(
        &" ".repeat(lead),
        line.trim_start_matches(' '),
        indent,
        width,
    )
}

/// Whitespace-separated tokens of `line` as (char offset, token) pairs.
fn token_offsets(line: &str) -> Vec<(usize, &str)> {
    let mut tokens = Vec::new();
    let mut token_start: Option<(usize, usize)> = None; // (char, byte)
    for (char_index, (byte, ch)) in line.char_indices().enumerate() {
        if ch.is_whitespace() {
            if let Some((start_char, start_byte)) = token_start.take() {
                tokens.push((start_char, &line[start_byte..byte]));
            }
        } else if token_start.is_none() {
            token_start = Some((char_index, byte));
        }
    }
    if let Some((start_char, start_byte)) = token_start {
        tokens.push((start_char, &line[start_byte..]));
    }
    tokens
}

/// Byte index of the `char_offset`-th char of `line` (its length when beyond).
fn byte_at(line: &str, char_offset: usize) -> usize {
    line.char_indices()
        .nth(char_offset)
        .map_or(line.len(), |(byte, _)| byte)
}

/// Char offset where a heading's blurb starts: the end of the first run of
/// two or more spaces, when any text follows it.
fn find_blurb_start(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let mut run = 0;
    for (index, ch) in chars.iter().enumerate() {
        if *ch == ' ' {
            run += 1;
            if run >= 2 {
                // End of this run.
                let mut end = index + 1;
                while end < chars.len() && chars[end] == ' ' {
                    end += 1;
                }
                if end < chars.len() {
                    return Some(end);
                }
                return None;
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Wrap `trailing` at word boundaries into lines of at most `width` chars:
/// the first follows `prefix`, continuations sit at `indent`. A word longer
/// than the available width is split so it still fits.
fn wrap_trailing(prefix: &str, trailing: &str, indent: usize, width: usize) -> String {
    let available_first = width.saturating_sub(prefix.chars().count()).max(1);
    let available_rest = width.saturating_sub(indent).max(1);
    let mut wrapped: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut available = available_first;
    let push_word =
        |current: &mut String, wrapped: &mut Vec<String>, available: &mut usize, word: &str| {
            let mut rest = word;
            loop {
                let room = available.saturating_sub(current.chars().count());
                if current.is_empty() {
                    if rest.chars().count() <= *available {
                        current.push_str(rest);
                        return;
                    }
                    // Split a word wider than a whole line.
                    let take = *available;
                    let split = byte_at(rest, take);
                    current.push_str(&rest[..split]);
                    wrapped.push(std::mem::take(current));
                    *available = available_rest;
                    rest = &rest[split..];
                } else if rest.chars().count() < room {
                    current.push(' ');
                    current.push_str(rest);
                    return;
                } else {
                    wrapped.push(std::mem::take(current));
                    *available = available_rest;
                }
            }
        };
    for word in trailing.split_whitespace() {
        push_word(&mut current, &mut wrapped, &mut available, word);
    }
    wrapped.push(current);
    let mut out = String::new();
    for (index, chunk) in wrapped.iter().enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
        } else {
            out.push_str(prefix);
        }
        out.push_str(chunk);
    }
    out
}
