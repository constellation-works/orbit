//! `orbit log tail` — column-formatted reader for the unified JSONL tracing
//! feed at `~/.orbit/state/logs/orbit.jsonl` (or wherever `--path` /
//! `ORBIT_LOG_PATH` points). Renders the v2-terminal-console mockup's four
//! columns: timestamp, source, code, message. Designed for human eyes by
//! default and pipeline-friendly when the sink disallows color (`--json` or
//! plain-text without ANSI escapes).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::format::{
    Filters, LevelFilter, build_filters as build_shared_filters, format_event_line,
    resolve_log_path,
};

#[derive(Args)]
pub struct TailArgs {
    /// Number of recent lines to print before exiting (or before tailing in
    /// follow mode).
    #[arg(short = 'n', long, default_value_t = 50)]
    pub lines: usize,

    /// Tail the file as it grows. Stop on Ctrl-C.
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Filter by tracing target prefix (e.g. `--target orbit.policy`
    /// matches `orbit.policy.deny`).
    #[arg(long)]
    pub target: Option<String>,

    /// Filter by minimum log level. `error > warn > info > debug > trace`.
    #[arg(long)]
    pub level: Option<LevelFilter>,

    /// Filter by timestamp window (e.g. `5m`, `1h`, `30s`, RFC3339).
    #[arg(long)]
    pub since: Option<String>,

    /// Emit each event as one raw JSON line instead of the four-column view.
    #[arg(long)]
    pub json: bool,

    /// Override the JSONL path. Falls back to `$ORBIT_LOG_PATH`, then
    /// `$HOME/.orbit/state/logs/orbit.jsonl`. Provided primarily for tests.
    #[arg(long)]
    pub path: Option<PathBuf>,
}

impl Execute for TailArgs {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        let path = resolve_log_path(self.path.as_deref())?;
        let filters = build_filters(&self)?;
        // A follow tail is unbounded, so its records cannot be collected into a
        // document before the first one is written. It is handed back as a
        // stream the renderer drives instead: that keeps the sink out of the
        // command (whether to colorize is the sink's answer, not this
        // command's — the local `is_terminal()` check that used to live here
        // ignored NO_COLOR and disagreed with every table-rendering command,
        // ADR-0308 §2) without pretending the output is a payload.
        let doc = json!({
            "path": path.to_string_lossy(),
            "follow": self.follow,
        });
        Ok(Payload::stream(
            doc,
            Box::new(move |sink, writer| {
                match run_tail(&path, &self, &filters, sink.color_allowed(), writer) {
                    Ok(()) => Ok(()),
                    // The reader closing the pipe is how `orbit log tail -f |
                    // head` ends, not a failure to report (spec §5).
                    Err(err) if crate::output::pipe::is_broken_pipe(&err) => Ok(()),
                    Err(err) => Err(io_to_orbit(err)),
                }
            }),
        )
        .into())
    }
}

pub(super) fn build_filters(args: &TailArgs) -> Result<Filters, OrbitError> {
    build_shared_filters(args.target.clone(), args.level, args.since.as_deref())
}

pub(super) fn run_tail<W: Write + ?Sized>(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
) -> io::Result<()> {
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("orbit log file not found: {}", path.display()),
        ));
    }

    let initial_offset = print_initial_window(path, args, filters, use_color, writer)?;
    if !args.follow {
        return Ok(());
    }

    follow_file(
        path,
        initial_offset,
        filters,
        args.json,
        use_color,
        writer,
        FollowControl::Forever,
    )
}

#[cfg(test)]
pub(super) struct FollowTestControl {
    ready: Sender<()>,
    stop: Receiver<()>,
}

#[cfg(test)]
impl FollowTestControl {
    pub(super) fn new(ready: Sender<()>, stop: Receiver<()>) -> Self {
        Self { ready, stop }
    }
}

#[cfg(test)]
pub(super) fn run_tail_with_test_control<W: Write + ?Sized>(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
    control: FollowTestControl,
) -> io::Result<()> {
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("orbit log file not found: {}", path.display()),
        ));
    }

    let initial_offset = print_initial_window(path, args, filters, use_color, writer)?;
    control.ready.send(()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "follow test stopped before readiness",
        )
    })?;
    if !args.follow {
        return Ok(());
    }

    follow_file(
        path,
        initial_offset,
        filters,
        args.json,
        use_color,
        writer,
        FollowControl::UntilStopped(control.stop),
    )
}

fn print_initial_window<W: Write + ?Sized>(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
) -> io::Result<u64> {
    let file = File::open(path)?;
    let total_bytes = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    let mut matching_lines = MatchingLineWindow::new(args.lines);
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        if args.lines > 0
            && let Ok(value) = serde_json::from_str::<Value>(&buf)
            && filters.matches(&value)
        {
            matching_lines.push(buf.trim_end_matches('\n').to_owned());
        }
    }

    for line in matching_lines.into_lines() {
        emit_line(&line, args.json, use_color, writer)?;
    }
    Ok(total_bytes)
}

/// A chronological tail window whose storage never exceeds its requested
/// record count. The line currently being parsed is held by the caller.
pub(super) struct MatchingLineWindow {
    limit: usize,
    lines: VecDeque<String>,
}

impl MatchingLineWindow {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit,
            lines: VecDeque::new(),
        }
    }

    pub(super) fn push(&mut self, line: String) {
        if self.limit == 0 {
            return;
        }
        if self.lines.len() == self.limit {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn into_lines(self) -> VecDeque<String> {
        self.lines
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.lines.len()
    }
}

fn follow_file<W: Write + ?Sized>(
    path: &Path,
    initial_offset: u64,
    filters: &Filters,
    json: bool,
    use_color: bool,
    writer: &mut W,
    control: FollowControl,
) -> io::Result<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(initial_offset))?;
    let mut reader = BufReader::new(file);
    let mut leftover = String::new();

    loop {
        if control.should_stop() {
            return Ok(());
        }
        let mut buf = String::new();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        if !buf.ends_with('\n') {
            // Partial line: stash and try again next iteration.
            leftover.push_str(&buf);
            continue;
        }
        let mut full_line = String::new();
        if !leftover.is_empty() {
            full_line.push_str(&leftover);
            leftover.clear();
        }
        full_line.push_str(buf.trim_end_matches('\n'));
        if let Ok(value) = serde_json::from_str::<Value>(&full_line)
            && filters.matches(&value)
        {
            emit_line(&full_line, json, use_color, writer)?;
        }
    }
}

enum FollowControl {
    Forever,
    #[cfg(test)]
    UntilStopped(Receiver<()>),
}

impl FollowControl {
    fn should_stop(&self) -> bool {
        match self {
            Self::Forever => false,
            #[cfg(test)]
            Self::UntilStopped(stop) => !matches!(stop.try_recv(), Err(TryRecvError::Empty)),
        }
    }
}

fn emit_line<W: Write + ?Sized>(
    raw: &str,
    json: bool,
    use_color: bool,
    writer: &mut W,
) -> io::Result<()> {
    if json {
        writeln!(writer, "{raw}")?;
        return Ok(());
    }
    let value = match serde_json::from_str::<Value>(raw) {
        Ok(v) => v,
        Err(_) => {
            // Skip malformed lines silently: the producer warns about cross-process
            // interleaves, and reader robustness is part of the JSONL contract.
            return Ok(());
        }
    };
    let formatted = format_event_line(&value, use_color);
    writeln!(writer, "{formatted}")
}

fn io_to_orbit(err: io::Error) -> OrbitError {
    OrbitError::InvalidInput(err.to_string())
}
