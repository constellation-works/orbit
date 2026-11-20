use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::tempdir;

use crate::command::log::tail::{
    FollowTestControl, TailArgs, build_filters, run_tail_with_test_control,
};

// Upper bound for every thread handshake and output wait. These only elapse on
// failure, so it is generous: an instrumented (coverage) build on a loaded
// runner can take seconds just to schedule the follower thread.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

fn write_fixture(path: &Path, lines: &[String]) {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(path, content).expect("write fixture");
}

fn make_args(path: PathBuf) -> TailArgs {
    TailArgs {
        lines: 50,
        follow: false,
        target: None,
        level: None,
        since: None,
        json_lines: false,
        path: Some(path),
    }
}

#[test]
fn append_during_initial_history_read_is_emitted_once_at_handoff() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    let first = json!({"target": "orbit.test", "fields": {"message": "first"}}).to_string();
    write_fixture(&path, std::slice::from_ref(&first));

    let mut args = make_args(path.clone());
    args.lines = 2;
    args.follow = true;
    args.json_lines = true;
    args.target = Some("orbit.test".to_string());
    let mut follower = spawn_follower_with_args(args, Duration::ZERO, true);
    follower.wait_until_paused();

    let during = json!({"target": "orbit.test", "fields": {"message": "during"}}).to_string();
    let ignored = json!({"target": "orbit.other", "fields": {"message": "ignored"}});
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append fixture");
    writeln!(file, "{during}\n{ignored}").expect("append during initial read");
    follower.resume_initial_read();
    follower.wait_until_ready();

    let sentinel = json!({"target": "orbit.test", "fields": {"message": "sentinel"}}).to_string();
    writeln!(file, "{sentinel}").expect("append after handoff");
    let output = follower.collect_through("sentinel");
    follower.finish();
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        [first, during, sentinel]
    );
}

// The initial-read hook forces the rotation/handoff race deterministically.
#[cfg(unix)]
#[test]
fn rotation_during_initial_read_keeps_the_old_reader_and_restarts_the_new_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    let first = json!({"fields": {"message": "first"}}).to_string();
    write_fixture(&path, std::slice::from_ref(&first));
    let mut archive_writer = OpenOptions::new().append(true).open(&path).unwrap();

    let mut args = make_args(path.clone());
    args.follow = true;
    args.json_lines = true;
    let mut follower = spawn_follower_with_args(args, Duration::ZERO, true);
    follower.wait_until_paused();

    std::fs::rename(&path, dir.path().join("archive.jsonl")).unwrap();
    let old = json!({"fields": {"message": "old-file-rest"}}).to_string();
    writeln!(archive_writer, "{old}").unwrap();
    let new = json!({"fields": {"message": "new-file-start"}}).to_string();
    write_fixture(&path, std::slice::from_ref(&new));
    follower.resume_initial_read();
    follower.wait_until_ready();

    let output = follower.collect_through("new-file-start");
    follower.finish();
    assert_eq!(output.lines().collect::<Vec<_>>(), [first, old, new]);
}

fn spawn_follower_with_args(
    args: TailArgs,
    startup_delay: Duration,
    pause_initial_read: bool,
) -> FollowWorker {
    // The worker owns the resume sender so that dropping it (for example when
    // the test panics while the reader is paused) disconnects the reader's
    // resume wait instead of leaving it to run out its deadline.
    let (initial_read_pause, reached_rx, resume_tx) = if pause_initial_read {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        (
            Some((reached_tx, resume_rx)),
            Some(reached_rx),
            Some(resume_tx),
        )
    } else {
        (None, None, None)
    };
    let (output_tx, output_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        thread::sleep(startup_delay);
        let mut buf = TeeWriter::new(output_tx);
        let path = args.path.as_ref().expect("fixture path");
        let filters = build_filters(&args).expect("filters");
        let mut control = FollowTestControl::new(ready_tx, stop_rx);
        if let Some((reached, resume)) = initial_read_pause {
            control = control.pause_during_initial_read(reached, resume);
        }
        run_tail_with_test_control(path, &args, &filters, false, &mut buf, control)
    });
    FollowWorker {
        output_rx,
        ready_rx,
        stop_tx,
        reached_rx,
        resume_tx,
        handle: Some(handle),
    }
}

struct FollowWorker {
    output_rx: mpsc::Receiver<String>,
    ready_rx: mpsc::Receiver<()>,
    stop_tx: mpsc::Sender<()>,
    reached_rx: Option<mpsc::Receiver<()>>,
    resume_tx: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<io::Result<()>>>,
}

impl FollowWorker {
    fn wait_until_paused(&self) {
        self.reached_rx
            .as_ref()
            .expect("follower was spawned with an initial-read pause")
            .recv_timeout(HANDSHAKE_TIMEOUT)
            .expect("initial reader paused");
    }

    fn resume_initial_read(&mut self) {
        self.resume_tx
            .take()
            .expect("initial reader has not been resumed")
            .send(())
            .expect("resume initial reader");
    }

    fn wait_until_ready(&self) {
        self.ready_rx
            .recv_timeout(HANDSHAKE_TIMEOUT)
            .expect("follower established its initial offset");
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<String, mpsc::RecvTimeoutError> {
        self.output_rx.recv_timeout(timeout)
    }

    fn collect_through(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut output = String::new();
        while Instant::now() < deadline {
            match self.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    output.push_str(&chunk);
                    if output.lines().any(|line| line.contains(needle)) {
                        return output;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.panic_on_disconnected_output(needle, &output);
                }
            }
        }
        panic!("follow output did not contain {needle}: {output}");
    }

    fn panic_on_disconnected_output(&mut self, needle: &str, output: &str) -> ! {
        let Some(handle) = self.handle.take() else {
            panic!("follower output channel disconnected before emitting {needle}: {output}");
        };
        match handle.join() {
            Ok(Ok(())) => {
                panic!("follower exited cleanly before emitting {needle}: {output}");
            }
            Ok(Err(error)) => {
                panic!("follower failed before emitting {needle}: {error}: {output}");
            }
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string panic");
                panic!("follower thread panicked before emitting {needle}: {message}: {output}");
            }
        }
    }

    fn finish(&mut self) {
        let _ = self.stop_tx.send(());
        self.join();
    }

    fn join(&mut self) {
        self.handle
            .take()
            .expect("follower handle is present")
            .join()
            .expect("follower thread did not panic")
            .expect("follower exited cleanly");
    }
}

impl Drop for FollowWorker {
    fn drop(&mut self) {
        // Disconnect a still-paused initial reader before joining it.
        self.resume_tx.take();
        let _ = self.stop_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TeeWriter {
    tx: mpsc::Sender<String>,
}

impl TeeWriter {
    fn new(tx: mpsc::Sender<String>) -> Self {
        Self { tx }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(text) = std::str::from_utf8(buf) {
            let _ = self.tx.send(text.to_string());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
