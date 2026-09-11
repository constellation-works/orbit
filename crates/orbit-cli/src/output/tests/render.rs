//! The renderer's projection of a payload into each mode
//! (`docs/design/terminal-interface/specs/output-modes.md` §3, ORB-10586).
//!
//! These assert the parts of the contract that are decided *after* the command
//! body returns, and so cannot be observed by testing a command: the plain
//! form's shape, when uniform-column suppression applies, and that `ndjson`
//! splits the same document `json` emits whole.
//!
//! Sinks are built through [`OutputSink::resolve`] with explicit inputs rather
//! than [`OutputSink::from_process`] — `make ci` has no TTY, so a sink taken
//! from the ambient environment would assert the piped path while appearing to
//! assert the terminal one.

use serde_json::json;

use crate::output::sink::{FormatArg, OutputMode, OutputSink, SinkEnv};
use crate::output::table::{Column, Table};

/// Two records that agree on STATUS and differ on NAME: STATUS is exactly the
/// column `specs/table-rendering.md` §5 suppresses when nothing was filtered.
fn uniform_status_table() -> Table {
    let mut table = Table::new(vec![
        Column::new("NAME").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("SUMMARY"),
    ]);
    table.add_row(vec!["alpha", "active", "the first record"]);
    table.add_row(vec!["beta", "active", "the second record"]);
    table
}

fn sink(is_tty: bool, requested: Option<FormatArg>) -> OutputSink {
    OutputSink::resolve(is_tty, &SinkEnv::default(), None, requested, false)
}

#[test]
fn auto_on_a_pipe_resolves_to_the_plain_form() {
    assert_eq!(sink(false, None).mode(), OutputMode::Plain);
    assert_eq!(sink(true, None).mode(), OutputMode::Table);
}

/// Spec §2: plain is the `cut -f` form — no header, single tabs, one line per
/// record.
#[test]
fn plain_has_no_header_and_separates_fields_with_one_tab() {
    let rendered = uniform_status_table().render_plain(&sink(false, None));
    let lines: Vec<&str> = rendered.lines().collect();

    assert_eq!(
        lines.len(),
        2,
        "header leaked into the plain form: {lines:?}"
    );
    assert_eq!(lines[0], "alpha\tactive\tthe first record");
    assert!(
        !rendered.contains("  "),
        "plain padded its columns: {rendered:?}"
    );
    assert!(
        !rendered.contains('\u{1b}'),
        "plain emitted ANSI: {rendered:?}"
    );
}

/// Spec §5: uniform-value suppression is a readability heuristic for the
/// `table` rendering. The plain form a pipe receives keeps every column,
/// whatever the values happen to be, so `cut -f4` names the same field for one
/// result set as for the next [ORB-12113].
#[test]
fn plain_keeps_a_uniform_column_the_table_rendering_drops() {
    let piped = sink(false, None);
    assert_eq!(piped.mode(), OutputMode::Plain);
    assert!(
        !piped.suppress_uniform_columns(),
        "the plain form suppressed a column"
    );
    assert_eq!(
        uniform_status_table().render_plain(&piped).lines().next(),
        Some("alpha\tactive\tthe first record"),
    );

    let terminal = sink(true, None);
    assert!(terminal.suppress_uniform_columns());
    let rendered = uniform_status_table().render_at(
        terminal.truncate_width(),
        terminal.color_allowed(),
        terminal.suppress_uniform_columns(),
    );
    assert!(
        !rendered.body.contains("STATUS"),
        "auto kept the uniform STATUS column: {}",
        rendered.body
    );
}

/// The `auto` rung keeps the readability heuristic; naming `table` explicitly
/// asks for the table's full shape and turns it off (spec §5).
#[test]
fn explicit_format_table_keeps_a_uniform_column_that_auto_drops() {
    let explicit = sink(false, Some(FormatArg::Table));
    assert!(!explicit.suppress_uniform_columns());
    let rendered = uniform_status_table().render_at(
        explicit.truncate_width(),
        explicit.color_allowed(),
        explicit.suppress_uniform_columns(),
    );
    assert!(
        rendered.body.contains("STATUS"),
        "--format table dropped a uniform column: {}",
        rendered.body
    );
}

/// A table's own opt-out is independent of the sink's: a fixed-shape view stays
/// whole even under `auto`, where a missing column would read as a missing
/// field.
#[test]
fn a_fixed_shape_view_keeps_every_column_under_auto() {
    let terminal = sink(true, None);
    assert!(
        terminal.suppress_uniform_columns(),
        "auto on a terminal is the sink answer under test"
    );
    let rendered = uniform_status_table().keep_all_columns().render_at(
        None,
        false,
        terminal.suppress_uniform_columns(),
    );
    assert!(rendered.body.contains("STATUS"), "{}", rendered.body);
}

/// Spec §3: a plain sink has no width, so nothing is truncated on its way into
/// a pipe — the failure the `width == 0` encoding exists to prevent.
#[test]
fn plain_never_truncates() {
    let long = format!("alpha-{}", "x".repeat(400));
    let other = format!("beta-{}", "x".repeat(400));
    let mut table = Table::new(vec![Column::new("NAME").fixed(), Column::new("BODY")]);
    table.add_row(vec!["alpha", long.as_str()]);
    table.add_row(vec!["beta", other.as_str()]);

    let rendered = table.render_plain(&sink(false, None));
    assert!(rendered.contains(&long), "plain truncated a value");
    assert!(!rendered.contains('…'), "plain emitted an ellipsis");
}

/// `--json` (rung 2) stays pretty wherever it lands, because every branch it
/// replaced called `print_pretty` unconditionally; `--format json` follows the
/// spec and pretty-prints only for a human.
#[test]
fn legacy_json_stays_pretty_on_a_pipe_but_format_json_does_not() {
    let legacy = OutputSink::resolve(false, &SinkEnv::default(), None, None, true);
    assert_eq!(legacy.mode(), OutputMode::Json);
    assert!(legacy.pretty_json(), "--json stopped pretty-printing");

    let explicit = sink(false, Some(FormatArg::Json));
    assert!(
        !explicit.pretty_json(),
        "--format json pretty-printed for a pipe"
    );
}

/// An explicit `--format` is rung 1 and outranks the legacy `--json` boolean;
/// `ORBIT_FORMAT` is rung 3 and does not.
#[test]
fn format_outranks_legacy_json_and_orbit_format_does_not() {
    let env = SinkEnv {
        format: Some("table".to_string()),
        ..SinkEnv::default()
    };
    assert_eq!(
        OutputSink::resolve(false, &env, None, None, true).mode(),
        OutputMode::Json,
        "ORBIT_FORMAT outranked --json"
    );
    // `--format table` names its rendering, so it stays `table` even on a
    // pipe — only `auto` resolves against the sink (spec §2).
    assert_eq!(
        OutputSink::resolve(
            false,
            &SinkEnv::default(),
            None,
            Some(FormatArg::Table),
            true
        )
        .mode(),
        OutputMode::Table,
        "--format did not outrank --json"
    );
}

/// `ndjson` splits exactly the records `json` emits as one array, and a detail
/// document is one record rather than being flattened into its fields.
#[test]
fn ndjson_records_are_the_json_documents_elements() {
    let list = json!([{ "id": "alpha" }, { "id": "beta" }]);
    assert_eq!(crate::output::render::ndjson_records(&list).len(), 2);

    let detail = json!({ "id": "alpha", "tags": ["a", "b"] });
    let records = crate::output::render::ndjson_records(&detail);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0], detail);
}
