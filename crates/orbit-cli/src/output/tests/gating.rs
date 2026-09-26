//! The property the sink exists to establish: one decision about color, and
//! one about width, applied identically by both styling backends.
//!
//! `NO_COLOR=1` on a terminal used to be honored by `colored` (the line-
//! rendering paths) and ignored by `comfy_table` (the table-rendering ones), so
//! a redirect captured escape sequences from half the CLI. Each test here pins
//! one half against the other.

use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use serde_json::json;

use crate::command::log::format::format_event_line;
use crate::output::sink::{OutputSink, SinkEnv};
use crate::output::table::{Column, Table};

/// The escape byte every ANSI sequence starts with.
const ESC: char = '\u{1b}';

/// crossterm caches the process environment, so table color tests run in an
/// isolated child with the forced-color environment from the terminal spec.
pub(super) fn run_in_forced_color_child(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_TABLE_BASIC_COLOR_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let output = Command::new(std::env::current_exe().expect("test binary path"))
        .args(["--exact", test_name, "--nocapture"])
        .env_remove("NO_COLOR")
        .env("TERM", "xterm")
        .env("CLICOLOR_FORCE", "1")
        .env(CHILD, "1")
        .output()
        .expect("run forced-color test child");
    assert!(
        output.status.success(),
        "forced-color child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// `colored`'s override is process-global, so tests that flip it run one at a
/// time. Tests that only compare two renderings against each other do not need
/// this lock — they agree under either global setting.
static COLOR_OVERRIDE: Mutex<()> = Mutex::new(());

/// Applies a sink's color policy for the duration of the test and restores
/// `colored`'s own detection afterwards, on panic as well as on success.
struct ColorPolicy {
    /// Held, not read: dropping it releases [`COLOR_OVERRIDE`].
    _lock: MutexGuard<'static, ()>,
}

impl ColorPolicy {
    fn apply(sink: &OutputSink) -> Self {
        let lock = COLOR_OVERRIDE.lock().unwrap_or_else(|err| err.into_inner());
        sink.apply_color_policy();
        Self { _lock: lock }
    }
}

impl Drop for ColorPolicy {
    fn drop(&mut self) {
        colored::control::unset_override();
    }
}

/// A terminal wide enough that nothing truncates, with `NO_COLOR` set.
fn no_color_terminal() -> OutputSink {
    let env = SinkEnv {
        no_color: Some("1".to_string()),
        columns: Some("200".to_string()),
        ..SinkEnv::default()
    };
    OutputSink::resolve(true, &env, Some(200), None, false)
}

/// A terminal with nothing suppressing color.
fn plain_terminal() -> OutputSink {
    OutputSink::resolve(true, &SinkEnv::default(), Some(200), None, false)
}

/// Stdout redirected to a file: not a terminal, so no width and no color.
fn redirected() -> OutputSink {
    OutputSink::resolve(false, &SinkEnv::default(), None, None, false)
}

fn render_table(sink: &OutputSink) -> String {
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("TITLE"),
    ]);
    table.add_row(vec![
        comfy_table::Cell::new("ORB-1"),
        crate::output::color::cell("done", crate::output::color::Domain::TaskStatus),
        comfy_table::Cell::new("first"),
    ]);
    table.add_row(vec![
        comfy_table::Cell::new("ORB-2"),
        crate::output::color::cell("blocked", crate::output::color::Domain::TaskStatus),
        comfy_table::Cell::new("second"),
    ]);
    table
        .render_at(sink.truncate_width(), sink.color_allowed(), true)
        .body
}

fn log_event() -> serde_json::Value {
    json!({
        "timestamp": "2026-08-02T03:58:00Z",
        "level": "ERROR",
        "target": "orbit.policy.deny",
        "fields": { "tool": "orbit.task.add", "path": "/tmp/x" },
    })
}

// ── The asymmetry this task exists to close ─────────────────────────────────

#[test]
fn no_color_renders_a_table_byte_for_byte_like_a_redirect() {
    let suppressed = render_table(&no_color_terminal());
    let piped = render_table(&redirected());

    assert_eq!(
        suppressed, piped,
        "NO_COLOR=1 on a terminal must produce exactly what a file gets"
    );
    assert!(
        !suppressed.contains(ESC),
        "no escape sequence survives: {suppressed:?}"
    );
}

#[test]
fn no_color_renders_a_line_byte_for_byte_like_a_redirect() {
    let event = log_event();

    let suppressed = {
        let sink = no_color_terminal();
        let _policy = ColorPolicy::apply(&sink);
        format_event_line(&event, sink.color_allowed())
    };
    let piped = {
        let sink = redirected();
        let _policy = ColorPolicy::apply(&sink);
        format_event_line(&event, sink.color_allowed())
    };

    assert_eq!(
        suppressed, piped,
        "the `colored`-backed paths must agree with the `comfy_table`-backed ones"
    );
    assert!(
        !suppressed.contains(ESC),
        "no escape sequence survives: {suppressed:?}"
    );
}

#[test]
fn a_terminal_without_no_color_still_gets_both_kinds_of_styling() {
    // Without this the two tests above would pass on a renderer that never
    // emits color at all.
    let sink = plain_terminal();
    let _policy = ColorPolicy::apply(&sink);

    assert!(
        render_table(&sink).contains(ESC),
        "a table on a color-allowed terminal is styled"
    );
    assert!(
        format_event_line(&log_event(), sink.color_allowed()).contains(ESC),
        "a log line on a color-allowed terminal is styled"
    );
}

// ── Width comes from the sink ───────────────────────────────────────────────

#[test]
fn a_zero_width_sink_truncates_nothing() {
    let sink = redirected();
    assert_eq!(sink.width(), 0);
    assert_eq!(
        sink.truncate_width(),
        None,
        "0 means `do not truncate`, never `truncate to nothing`"
    );

    let mut table = Table::new(vec![Column::new("TITLE")]);
    let long = "a title far longer than any terminal would ever be, repeated \
                until it could not possibly fit inside eighty or even two \
                hundred columns of a real terminal window";
    table.add_row(vec![comfy_table::Cell::new(long)]);
    table.add_row(vec![comfy_table::Cell::new("short")]);

    let rendered = table.render_at(sink.truncate_width(), sink.color_allowed(), true);

    assert!(
        rendered.body.contains(long),
        "a redirected list carries whole values: {}",
        rendered.body
    );
    assert!(rendered.notices.is_empty(), "nothing was dropped to fit");
}

#[test]
fn a_sized_sink_truncates_to_its_width() {
    // `NO_COLOR` so a line's byte count is its column count; the width under
    // test is independent of whether the sink is styled.
    let env = SinkEnv {
        columns: Some("40".to_string()),
        no_color: Some("1".to_string()),
        ..SinkEnv::default()
    };
    let sink = OutputSink::resolve(true, &env, Some(200), None, false);

    assert_eq!(
        sink.truncate_width(),
        Some(40),
        "COLUMNS wins over the terminal's own 200"
    );

    let mut table = Table::new(vec![Column::new("TITLE")]);
    table.add_row(vec![comfy_table::Cell::new(
        "a title far longer than forty columns of terminal",
    )]);
    table.add_row(vec![comfy_table::Cell::new("short")]);

    let rendered = table.render_at(sink.truncate_width(), sink.color_allowed(), true);

    for line in rendered.body.lines() {
        assert!(line.chars().count() <= 40, "line exceeds the sink: {line}");
    }
}

// ── Progress (spec §6) ──────────────────────────────────────────────────────

#[test]
fn progress_needs_a_terminal_and_a_human_facing_mode() {
    use crate::output::sink::FormatArg;

    assert!(
        plain_terminal().progress_allowed(),
        "an interactive table run may draw progress"
    );
    assert!(
        !redirected().progress_allowed(),
        "a redirected stream never does"
    );
    for format in [FormatArg::Json, FormatArg::Ndjson] {
        let sink = OutputSink::resolve(true, &SinkEnv::default(), Some(200), Some(format), false);
        assert!(
            !sink.progress_allowed(),
            "{format:?} is chosen by a consumer who is not watching, TTY or not"
        );
    }
}
