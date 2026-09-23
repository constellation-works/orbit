//! Rendering assertions for the borderless table contract (ADR-0307).
//!
//! Every case pins the width explicitly rather than reading `COLUMNS`, so the
//! geometry is the same under `cargo test` and under `--nocapture`.
//!
//! ## Why the "table" form's golden coverage lives here (ORB-10571)
//!
//! `Table::print` decides whether to truncate and style by calling
//! `std::io::stdout().is_terminal()` itself, and comfy-table's own
//! `should_style` performs the same check independently — so a subprocess
//! test, whose stdout is always a pipe, can never observe genuine "table"
//! rendering (truncated, at a real width) no matter what `--format` or
//! `COLUMNS` it passes. [`Table::render`] is called directly here instead,
//! at an explicit width, which is the only way to exercise that path at all.
//! The plain and `json` forms *are* reachable end-to-end and are golden-
//! tested against the real binary in `tests/output_goldens.rs`.
//!
//! A corollary, also worth recording: `styled: true` cannot be observed to
//! differ from `styled: false` in this binary either, for the same reason —
//! comfy-table's `should_style` will not emit ANSI outside a real terminal
//! regardless of the flag this crate passes it. Every fixture below renders
//! with `styled: false`, matching what the flag can actually be shown to do
//! here.

use std::path::{Path, PathBuf};

use crate::output::table::{Column, Table, build_table};

const BOX_GLYPHS: &[char] = &['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '+'];

/// A result set with values long enough to force truncation at any sane width.
fn tool_list() -> Table {
    let mut table = Table::new(vec![
        Column::new("NAME").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("BUILTIN").fixed(),
        Column::new("DESCRIPTION"),
    ]);
    table.add_row(vec![
        "orbit.task.locks.reserve",
        "inactive",
        "yes",
        "Reserve context-file locks for a task. Exactly one of `task_ids` or `files` must be given.",
    ]);
    table.add_row(vec![
        "orbit.task.show",
        "active",
        "yes",
        "Show one task with its comments, history, and artifacts.",
    ]);
    table
}

fn lines(rendered: &str) -> Vec<&str> {
    rendered.lines().collect()
}

#[test]
fn renders_one_line_per_record_with_a_header_and_no_box_glyphs() {
    let table = tool_list();

    for width in [None, Some(200), Some(120), Some(80), Some(40)] {
        let rendered = table.render_at(width, false, true);
        let lines = lines(&rendered.body);
        assert_eq!(
            lines.len(),
            3,
            "2 records plus one header at width {width:?}: {}",
            rendered.body
        );
        assert!(
            !rendered.body.chars().any(|c| BOX_GLYPHS.contains(&c)),
            "no box drawing at width {width:?}: {}",
            rendered.body
        );
    }
}

#[test]
fn columns_are_two_spaces_apart_with_no_leading_indent() {
    let mut table = build_table(&["ID", "TITLE"]);
    table.add_row(vec!["T1", "first"]);
    table.add_row(vec!["T2", "second"]);

    let rendered = table.render_at(None, false, true);

    assert_eq!(rendered.body.lines().next(), Some("ID  TITLE"));
    assert_eq!(rendered.body.lines().nth(1), Some("T1  first"));
    // Trailing padding is trimmed, so `cut`/`grep` see no phantom field.
    assert_eq!(rendered.body.lines().nth(2), Some("T2  second"));
}

#[test]
fn overflow_is_truncated_with_a_single_ellipsis() {
    let rendered = tool_list().render_at(Some(80), false, true);

    assert!(
        rendered.body.contains('…'),
        "long descriptions must truncate at 80 columns: {}",
        rendered.body
    );
    for line in lines(&rendered.body) {
        assert!(
            line.chars().filter(|c| *c == '…').count() <= 1,
            "one ellipsis per line: {line}"
        );
        assert!(
            line.chars().count() <= 80,
            "line exceeds the pinned width: {line}"
        );
    }
}

#[test]
fn fixed_columns_keep_their_width_while_flexible_ones_shrink() {
    let table = tool_list();

    let wide = table.render_at(Some(200), false, true);
    let narrow = table.render_at(Some(60), false, true);

    for rendered in [&wide, &narrow] {
        assert!(
            rendered.body.contains("orbit.task.locks.reserve"),
            "a fixed identifier column is never squeezed: {}",
            rendered.body
        );
    }
    assert!(
        !narrow
            .body
            .contains("Reserve context-file locks for a task."),
        "the flexible description column absorbs the squeeze: {}",
        narrow.body
    );
}

#[test]
fn flexible_columns_drop_from_the_right_and_say_so_on_stderr() {
    // The fixed columns plus a floored DESCRIPTION need 44 columns, so the
    // flexible one is dropped rather than squeezed below its floor.
    let rendered = tool_list().render_at(Some(40), false, true);

    assert!(
        !rendered.body.contains("DESCRIPTION"),
        "the rightmost flexible column is dropped: {}",
        rendered.body
    );
    assert_eq!(
        rendered.notices,
        vec!["columns hidden to fit the terminal: DESCRIPTION".to_string()],
        "a dropped column is reported, on stderr, never in the body"
    );
    assert_eq!(lines(&rendered.body).len(), 3, "records are not dropped");
}

#[test]
fn a_column_with_one_value_across_every_row_is_suppressed() {
    let rendered = tool_list().render_at(None, false, true);

    assert!(
        !rendered.body.contains("BUILTIN"),
        "every tool is builtin, so the column carries nothing: {}",
        rendered.body
    );
    assert!(
        rendered.body.contains("STATUS"),
        "status varies across the result set and stays: {}",
        rendered.body
    );
}

#[test]
fn a_suppressed_column_survives_when_the_caller_filtered_on_it() {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed().filtered(true),
    ]);
    table.add_row(vec!["T1", "done"]);
    table.add_row(vec!["T2", "done"]);

    let rendered = table.render_at(None, false, true);

    assert!(
        rendered.body.starts_with("ID  STATUS"),
        "`--status done` keeps STATUS on screen: {}",
        rendered.body
    );
}

#[test]
fn keep_all_columns_opts_a_fixed_shape_view_out_of_suppression() {
    let mut table = build_table(&["COMPONENT", "CURRENT"]).keep_all_columns();
    table.add_row(vec!["workspace layout", "3"]);
    table.add_row(vec!["store schema", "3"]);

    let rendered = table.render_at(None, false, true);

    assert!(
        rendered.body.contains("CURRENT"),
        "a repeated version column still names itself: {}",
        rendered.body
    );
    assert_eq!(lines(&rendered.body).len(), 3);
}

#[test]
fn a_single_record_keeps_every_column() {
    let mut table = build_table(&["ID", "STATUS"]);
    table.add_row(vec!["T1", "done"]);

    let rendered = table.render_at(None, false, true);

    assert_eq!(lines(&rendered.body), vec!["ID  STATUS", "T1  done"]);
}

#[test]
fn numeric_columns_are_right_aligned_under_a_unit_header() {
    let mut table = Table::new(vec![
        Column::new("#").number(),
        Column::new("STATE").fixed(),
        Column::new("DURATION (ms)").number(),
    ]);
    table.add_row(vec!["1", "success", "187"]);
    table.add_row(vec!["2", "failed", "42910"]);

    let rendered = table.render_at(None, false, true);

    assert_eq!(
        lines(&rendered.body),
        vec![
            "#  STATE    DURATION (ms)",
            "1  success            187",
            "2  failed           42910",
        ],
        "magnitudes line up on their right edge: {}",
        rendered.body
    );
}

#[test]
fn a_path_loses_its_middle_rather_than_its_identifying_tail() {
    let mut table = Table::new(vec![
        Column::new("PATH").path(),
        Column::new("TYPE").fixed(),
    ]);
    table.add_row(vec![
        "docs/design/terminal-interface/specs/table-rendering.md",
        "design",
    ]);
    table.add_row(vec!["docs/design/terminal-interface/1_overview.md", "spec"]);

    let rendered = table.render_at(Some(40), false, true);

    assert!(
        rendered.body.contains("le-rendering.md") && rendered.body.contains("1_overview.md"),
        "the identifying tail survives: {}",
        rendered.body
    );
    assert!(
        rendered.body.contains("docs/design/term…"),
        "the head survives and the middle is elided: {}",
        rendered.body
    );
}

#[test]
fn a_multi_line_value_still_occupies_exactly_one_line() {
    let mut table = build_table(&["ID", "SUMMARY"]);
    table.add_row(vec!["T1", "first line\nsecond line\nthird line"]);
    table.add_row(vec!["T2", "single line"]);

    let rendered = table.render_at(None, false, true);

    assert_eq!(lines(&rendered.body).len(), 3);
    assert!(
        !rendered.body.contains("second line"),
        "the wrapped remainder is truncated, not printed: {}",
        rendered.body
    );
}

// --- ORB-10571: golden coverage for the "table" form of three real list
// commands, at a pinned width. Each fixture below reproduces the column
// layout its command builds (cross-checked against the source at the call
// site named in its doc comment) fed with representative row data; this
// mirrors `tool_list` above rather than reaching into `command::task`,
// or `command::skill`, which have no seam that returns a
// `Table` before printing it.

/// Mirrors `command::task::output::print_task_table`'s default (non
/// `--full`) column set: `orbit task list` with no `--status`/`--priority`/
/// `--type` filter, so every column is unfiltered.
fn task_list() -> Table {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("TITLE"),
        Column::new("STATUS").fixed(),
        Column::new("PRIORITY").fixed(),
        Column::new("TYPE").fixed(),
    ]);
    table.add_row(vec![
        "ORB-00000",
        "Fix the flaky retry loop in the sync worker that drops events under backpressure",
        "proposed",
        "high",
        "bug",
    ]);
    table.add_row(vec![
        "ORB-00001",
        "Document the new sink resolution precedence for --format and ORBIT_FORMAT",
        "in-progress",
        "medium",
        "chore",
    ]);
    table.add_row(vec![
        "ORB-00002",
        "Add pagination to the audit export command",
        "proposed",
        "low",
        "feature",
    ]);
    table
}

/// Mirrors `command::skill::list::SkillListArgs::execute`'s column set.
fn skill_list() -> Table {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("HASH").fixed(),
        Column::new("TAGS").number(),
        Column::new("SUMMARY"),
    ]);
    table.add_row(vec![
        "orbit-search",
        "e3f03c1208",
        "3",
        "Search tasks and frictions through the unified orbit search query surface",
    ]);
    table.add_row(vec![
        "orbit-task",
        "a1ebb7a6f6",
        "2",
        "Create, execute, and review Orbit tasks through the lifecycle with explicit status tracking",
    ]);
    table
}

/// Narrow enough to force at least one flexible column to truncate in every
/// fixture above, per [`a_zero_width_truncates_nothing_while_a_narrow_width_truncates_with_one_ellipsis`].
const TABLE_GOLDEN_WIDTH: usize = 60;

fn golden_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/output_goldens")
        .join(file_name)
}

/// Compare against (or, with `ORBIT_UPDATE_OUTPUT_GOLDENS=1`, overwrite) the
/// checked-in golden file. Shares its golden directory and its regeneration
/// env var with `tests/output_goldens.rs`'s plain/`json` coverage of the
/// same commands, so one command regenerates every golden in this
/// crate: `ORBIT_UPDATE_OUTPUT_GOLDENS=1 cargo test -p orbit-cli`.
/// Regenerating is a deliberate act requiring review of the diff — not a
/// fix for a failing test.
fn assert_golden(file_name: &str, actual: &str) {
    let path = golden_path(file_name);
    if std::env::var("ORBIT_UPDATE_OUTPUT_GOLDENS").as_deref() == Ok("1") {
        std::fs::write(&path, actual)
            .unwrap_or_else(|err| panic!("write golden {}: {err}", path.display()));
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "cannot read {} ({err}); regenerate with \
             `ORBIT_UPDATE_OUTPUT_GOLDENS=1 cargo test -p orbit-cli`",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} drifted from its golden. If the new rendering is correct, regenerate with \
         `ORBIT_UPDATE_OUTPUT_GOLDENS=1 cargo test -p orbit-cli` and review the diff before \
         committing.",
        path.display()
    );
}

#[test]
fn table_form_matches_goldens_at_a_pinned_width() {
    let fixtures: [(&str, Table); 3] = [
        ("tool_list.table.txt", tool_list()),
        ("task_list.table.txt", task_list()),
        ("skill_list.table.txt", skill_list()),
    ];
    for (file_name, table) in &fixtures {
        let rendered = table.render_at(Some(TABLE_GOLDEN_WIDTH), false, true);
        assert_golden(file_name, &rendered.body);
    }
}

/// table-rendering.md §4: overflow truncates with a single `…`, and
/// "truncation never applies to ... the plain piped form." A `None` sink
/// width is what a non-terminal sink resolves to (output-modes.md §1's
/// `width == 0` invariant, represented here as `Option::None` rather than
/// the numeric zero — see this module's top-of-file note on why `table.rs`
/// does not yet consume `OutputSink` directly), and it must leave every
/// value whole.
#[test]
fn a_zero_width_truncates_nothing_while_a_narrow_width_truncates_with_one_ellipsis() {
    // A single truncated value carries exactly one ellipsis
    // (`overflow_is_truncated_with_a_single_ellipsis` above pins that per
    // cell); a row with more than one flexible column can validly carry more
    // than one ellipsis on the same line, so this test checks presence, not a
    // per-line count.
    let mut truncated_anywhere = false;
    for table in [tool_list(), task_list(), skill_list()] {
        let untruncated = table.render_at(None, false, true);
        assert!(
            !untruncated.body.contains('…'),
            "a sink with no width must not truncate: {}",
            untruncated.body
        );

        let narrow = table.render_at(Some(TABLE_GOLDEN_WIDTH), false, true);
        truncated_anywhere |= narrow.body.contains('…');
    }
    assert!(
        truncated_anywhere,
        "width {TABLE_GOLDEN_WIDTH} must be narrow enough to exercise truncation in at least \
         one fixture, or this test is not testing what its name says"
    );
}
