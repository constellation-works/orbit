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
                // Line shape follows the resolved sink, not the command-local
                // `--json` flag: `--format json|ndjson` must emit JSONL even
                // when that flag is absent.
                let mut args = self;
                args.json = matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
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
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("orbit log file not found: {}", path.display()),
        ));
    }

    let initial = print_initial_window(path, args, filters, use_color, writer)?;
    if !args.follow {
        return Ok(());
    }

    follow_file(
        path,
        initial,
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
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("orbit log file not found: {}", path.display()),
        ));
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
) -> io::Result<InitialWindow> {
    print_initial_window_with_hook(path, args, filters, use_color, writer, || Ok(()))
}

struct InitialWindow {
    reader: BufReader<File>,
    pending: Vec<u8>,
}

fn print_initial_window_with_hook<W: Write + ?Sized>(
    path: &Path,
    args: &TailArgs,
    filters: &Filters,
    use_color: bool,
    writer: &mut W,
    after_first_read: impl FnOnce() -> io::Result<()>,
) -> io::Result<InitialWindow> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut pending = Vec::new();
    let mut matching_lines = MatchingLineWindow::new(args.lines);
    let mut after_first_read = Some(after_first_read);
    loop {
        buf.clear();
        // Bytes, not `read_line`: one torn or non-UTF-8 line must not end the
        // whole tail.
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if let Some(hook) = after_first_read.take() {
            hook()?;
        }
        if args.follow && buf.last() != Some(&b'\n') {
            // The final record may be completed after follow starts. Its
            // bytes belong to the follow reader, even with zero history.
            pending = std::mem::take(&mut buf);
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        if args.lines > 0
            && let Ok(value) = serde_json::from_str::<Value>(&line)
            && filters.matches(&value)
        {
            matching_lines.push(line.trim_end_matches('\n').to_owned());
        }
    }

    for line in matching_lines.into_lines() {
        emit_line(&line, args.json, use_color, writer)?;
    }
    // Carry this exact file into follow mode: rotation during the history
    // read must not apply an old offset to the replacement path.
    Ok(InitialWindow { reader, pending })
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
    let mut reader = initial.reader;
    let mut offset = reader.stream_position()?;
    // Bytes of a line still being written. Kept undecoded so a write that
    // ends inside a multi-byte character is completed, not rejected.
    let mut pending = initial.pending;

    loop {
        if control.should_stop() {
            return Ok(());
        }
        if reader.get_ref().metadata()?.len() < offset {
            // A copy-truncate rotation invalidates both buffered bytes and
            // any unfinished record from the previous contents.
            reader.seek(SeekFrom::Start(0))?;
            offset = 0;
            pending.clear();
        }
        let n = reader.read_until(b'\n', &mut pending)?;
        if n == 0 {
            // Reach EOF on the old descriptor before switching, so its
            // complete records are drained even if the path was renamed.
            let replacement = match File::open(path) {
                Ok(file) => Some(file),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(err),
            };
            if let Some(file) = replacement {
                let current = reader.get_ref().metadata()?;
                if current.len() > offset {
                    // The old writer appended while we checked the path.
                    continue;
                }
                if !same_file(&current, &file.metadata()?)? {
                    reader = BufReader::new(file);
                    offset = 0;
                    // Never join a torn archive record to the new file.
                    pending.clear();
                    continue;
                }
            }
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        offset += n as u64;
        if pending.last() != Some(&b'\n') {
            // Partial line: keep it and try again next iteration.
            continue;
        }
        pending.pop();
        let full_line = String::from_utf8_lossy(&pending).into_owned();
        pending.clear();
        if let Ok(value) = serde_json::from_str::<Value>(&full_line)
            && filters.matches(&value)
        {
            emit_line(&full_line, json, use_color, writer)?;
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

fn io_to_orbit(err: io::Error) -> OrbitError {
    OrbitError::InvalidInput(err.to_string())
}
