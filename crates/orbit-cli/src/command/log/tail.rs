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
use crate::output::sink::OutputMode;

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

    /// Stream rendering, derived from the output sink rather than argv.
    #[arg(skip)]
    pub(super) json_lines: bool,

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
                // Line shape follows the resolved sink: JSON modes retain
                // the stream's established JSONL record format.
                let mut args = self;
                args.json_lines = matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
                match run_tail(&path, &args, &filters, sink.color_allowed(), writer) {
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

fn run_tail<W: Write + ?Sized>(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
) -> io::Result<()> {
    if let Some(agent_path) = orbit_common::observability::logging::agent_jsonl_log_path(path) {
        return run_split_tail(path, &agent_path, args, filters, use_color, writer);
    }
    if !path.exists() {
        return Err(log_file_not_found(path));
    }

    let initial = print_initial_window(path, args, filters, use_color, writer)?;
    if !args.follow {
        return Ok(());
    }

    follow_file(
        path,
        initial,
        filters,
        args.json_lines,
        use_color,
        writer,
        FollowControl::Forever,
    )
}

#[cfg(test)]
pub(super) struct FollowTestControl {
    ready: Sender<()>,
    stop: Receiver<()>,
    initial_read_pause: Option<(Sender<()>, Receiver<()>)>,
}

#[cfg(test)]
impl FollowTestControl {
    pub(super) fn new(ready: Sender<()>, stop: Receiver<()>) -> Self {
        Self {
            ready,
            stop,
            initial_read_pause: None,
        }
    }

    pub(super) fn pause_during_initial_read(
        mut self,
        reached: Sender<()>,
        resume: Receiver<()>,
    ) -> Self {
        self.initial_read_pause = Some((reached, resume));
        self
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
        return Err(log_file_not_found(path));
    }

    let initial = print_initial_window_with_hook(path, args, filters, use_color, writer, || {
        if let Some((reached, resume)) = control.initial_read_pause.as_ref() {
            reached.send(()).map_err(|_| io::ErrorKind::BrokenPipe)?;
            resume
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| io::ErrorKind::TimedOut)?;
        }
        Ok(())
    })?;
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
        initial,
        filters,
        args.json_lines,
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
) -> io::Result<InitialWindow> {
    print_initial_window_with_hook(path, args, filters, use_color, writer, || Ok(()))
}

struct InitialWindow {
    reader: BufReader<File>,
    pending: RecordBuffer,
}

// Excludes the newline delimiter. Oversized records are silently skipped,
// just like malformed JSON; keep discarding across EOF until their newline.
const MAX_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct RecordBuffer {
    bytes: Vec<u8>,
    discarding: bool,
}

impl RecordBuffer {
    /// Consume at most one reader-buffer chunk, retaining only admitted bytes.
    /// The caller clears the record after a delimiter or a file change.
    fn read_chunk(&mut self, reader: &mut impl BufRead) -> io::Result<(usize, bool)> {
        let available = reader.fill_buf()?;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let payload_len = newline.unwrap_or(available.len());
        let consumed = payload_len + usize::from(newline.is_some());
        if !self.discarding {
            if payload_len > MAX_RECORD_BYTES - self.bytes.len() {
                self.bytes.clear();
                self.discarding = true;
            } else {
                // Exact growth avoids geometric allocation above the byte cap.
                self.bytes.reserve_exact(payload_len);
                self.bytes.extend_from_slice(&available[..payload_len]);
            }
        }
        reader.consume(consumed);
        Ok((consumed, newline.is_some()))
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.discarding = false;
    }
}

fn print_initial_window_with_hook<W: Write + ?Sized>(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
    after_first_read: impl FnOnce() -> io::Result<()>,
) -> io::Result<InitialWindow> {
    let (initial, history) = read_initial_window(path, args, filters, after_first_read)?;
    for line in history {
        emit_line(&line, args.json_lines, use_color, writer)?;
    }
    Ok(initial)
}

fn read_initial_window(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    after_first_read: impl FnOnce() -> io::Result<()>,
) -> io::Result<(InitialWindow, VecDeque<String>)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut pending = RecordBuffer::default();
    let mut matching_lines = MatchingLineWindow::new(args.lines);
    let mut after_first_read = Some(after_first_read);
    loop {
        // Read incrementally even with zero history: neither complete nor
        // delimiter-free records may allocate beyond the admission bound.
        let (n, complete) = pending.read_chunk(&mut reader)?;
        if n == 0 && (args.follow || pending.bytes.is_empty() || pending.discarding) {
            break;
        }
        if let Some(hook) = after_first_read.take() {
            hook()?;
        }
        if n > 0 && !complete {
            continue;
        }
        // A one-shot tail still accepts a final record without a newline.
        // Follow carries both admitted partial bytes and discard state at EOF.
        let line = String::from_utf8_lossy(&pending.bytes);
        if args.lines > 0
            && !pending.discarding
            && let Ok(value) = serde_json::from_str::<Value>(&line)
            && filters.matches(&value)
        {
            matching_lines.push(line.into_owned());
        }
        pending.clear();
        if n == 0 {
            break;
        }
    }

    // Carry this exact file into follow mode: rotation during the history
    // read must not apply an old offset to the replacement path.
    Ok((
        InitialWindow { reader, pending },
        matching_lines.into_lines(),
    ))
}

/// A chronological tail window whose storage never exceeds its requested
/// record count. The line currently being parsed is held by the caller.
struct MatchingLineWindow {
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
}

fn follow_file<W: Write + ?Sized>(
    path: &Path,
    initial: InitialWindow,
    filters: &Filters,
    json: bool,
    use_color: bool,
    writer: &mut W,
    control: FollowControl,
) -> io::Result<()> {
    follow_files(
        vec![(path.to_path_buf(), Some(initial))],
        filters,
        json,
        use_color,
        writer,
        control,
    )
}

fn run_split_tail<W: Write + ?Sized>(
    path: &Path,
    agent_path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
) -> io::Result<()> {
    let mut feeds = Vec::new();
    let mut history = Vec::new();
    let mut opened = false;
    for path in [path, agent_path] {
        match read_initial_window(path, args, filters, || Ok(())) {
            Ok((initial, lines)) => {
                opened = true;
                history.extend(lines.into_iter().filter_map(|line| {
                    let event: Value = serde_json::from_str(&line).ok()?;
                    Some((event["timestamp"].as_str().unwrap_or("").to_owned(), line))
                }));
                feeds.push((path.to_path_buf(), Some(initial)));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                feeds.push((path.to_path_buf(), None));
            }
            Err(error) => return Err(error),
        }
    }
    // A one-shot read with neither feed on disk is a missing log, not an empty
    // one. Follow mode keeps waiting: either feed can appear on a fresh install.
    if !opened && !args.follow {
        return Err(log_file_not_found(path));
    }
    history.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, line) in history
        .iter()
        .skip(history.len().saturating_sub(args.lines))
    {
        emit_line(line, args.json_lines, use_color, writer)?;
    }
    if args.follow {
        follow_files(
            feeds,
            filters,
            args.json_lines,
            use_color,
            writer,
            FollowControl::Forever,
        )?;
    }
    Ok(())
}

fn follow_files<W: Write + ?Sized>(
    mut feeds: Vec<(PathBuf, Option<InitialWindow>)>,
    filters: &Filters,
    json: bool,
    use_color: bool,
    writer: &mut W,
    control: FollowControl,
) -> io::Result<()> {
    loop {
        if control.should_stop() {
            return Ok(());
        }
        let mut progressed = false;
        for (path, initial) in &mut feeds {
            if initial.is_none() {
                match File::open(&*path) {
                    Ok(file) => {
                        *initial = Some(InitialWindow {
                            reader: BufReader::new(file),
                            pending: RecordBuffer::default(),
                        })
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                }
            }
            let Some(initial) = initial else { continue };
            let reader = &mut initial.reader;
            let pending = &mut initial.pending;
            let mut offset = reader.stream_position()?;
            // Bound bytes as well as records per batch: each read consumes at
            // most one fixed-size BufReader chunk, including discarded input.
            for _ in 0..256 {
                if reader.get_ref().metadata()?.len() < offset {
                    reader.seek(SeekFrom::Start(0))?;
                    offset = 0;
                    pending.clear();
                }
                let (n, complete) = pending.read_chunk(reader)?;
                if n == 0 {
                    let replacement = match File::open(&*path) {
                        Ok(file) => Some(file),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                        Err(error) => return Err(error),
                    };
                    if let Some(file) = replacement {
                        let current = reader.get_ref().metadata()?;
                        if current.len() > offset {
                            continue;
                        }
                        if !same_file(&current, &file.metadata()?)? {
                            *reader = BufReader::new(file);
                            offset = 0;
                            pending.clear();
                            progressed = true;
                            continue;
                        }
                    }
                    break;
                }
                progressed = true;
                offset += n as u64;
                if !complete {
                    continue;
                }
                if pending.discarding {
                    pending.clear();
                    continue;
                }
                let full_line = String::from_utf8_lossy(&pending.bytes).into_owned();
                pending.clear();
                if let Ok(value) = serde_json::from_str::<Value>(&full_line)
                    && filters.matches(&value)
                {
                    emit_line(&full_line, json, use_color, writer)?;
                }
            }
        }
        if !progressed {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "following log rotation requires Unix file identity (use WSL2 on Windows)",
        ))
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

fn log_file_not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("orbit log file not found: {}", path.display()),
    )
}

fn io_to_orbit(err: io::Error) -> OrbitError {
    OrbitError::InvalidInput(err.to_string())
}
