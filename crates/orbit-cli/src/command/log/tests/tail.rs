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
        json: false,
        path: Some(path),
    }
}

#[test]
fn append_during_initial_history_read_is_emitted_once_at_handoff() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    let first = json!({"target": "orbit.test", "fields": {"message": "first"}}).to_string();
    write_fixture(&path, std::slice::from_ref(&first));

    let (reached_tx, reached_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let mut args = make_args(path.clone());
    args.lines = 2;
    args.follow = true;
    args.json = true;
    args.target = Some("orbit.test".to_string());
    let mut follower =
        spawn_follower_with_args(args, Duration::ZERO, Some((reached_tx, resume_rx)));
    reached_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial reader paused after first line");

    let during = json!({"target": "orbit.test", "fields": {"message": "during"}}).to_string();
    let ignored = json!({"target": "orbit.other", "fields": {"message": "ignored"}});
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append fixture");
    writeln!(file, "{during}\n{ignored}").expect("append during initial read");
    resume_tx.send(()).expect("resume initial reader");
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

fn spawn_follower_with_args(
    args: TailArgs,
    startup_delay: Duration,
    initial_read_pause: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> FollowWorker {
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
        handle: Some(handle),
    }
}

struct FollowWorker {
    output_rx: mpsc::Receiver<String>,
    ready_rx: mpsc::Receiver<()>,
    stop_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<io::Result<()>>>,
}

impl FollowWorker {
    fn wait_until_ready(&self) {
        self.ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("follower established its initial offset");
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<String, mpsc::RecvTimeoutError> {
        self.output_rx.recv_timeout(timeout)
    }

    fn collect_through(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut output = String::new();
        while Instant::now() < deadline {
            if let Ok(chunk) = self.recv_timeout(Duration::from_millis(50)) {
                output.push_str(&chunk);
                if output.lines().any(|line| line.contains(needle)) {
                    return output;
                }
            }
        }
        panic!("follow output did not contain {needle}: {output}");
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
