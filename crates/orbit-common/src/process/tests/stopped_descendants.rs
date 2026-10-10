//! A pid that now names a different process must never be signalled. A real
//! pid reuse cannot be arranged on demand, so the start identity the watch
//! recorded is falsified instead.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::process::identity::linux_process_stat;
use crate::process::stopped_descendants::linux::end_stopped;

struct ReapingChild(Child);

impl Drop for ReapingChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_stopped_pid_with_another_start_identity_is_never_signalled() {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "kill -STOP $$"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = ReapingChild(command.spawn().expect("spawn self-stopping shell"));
    let pid = child.0.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let stat = loop {
        match linux_process_stat(pid) {
            Some(stat) if stat.state == 'T' => break stat,
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            other => panic!("shell never stopped: {other:?}"),
        }
    };

    let reused = end_stopped(pid, stat.start_ticks + 1, Duration::from_secs(1));
    assert_eq!(reused, None, "a pid with another start time is not ours");
    assert_eq!(
        linux_process_stat(pid).map(|stat| stat.state),
        Some('T'),
        "the process the pid names now must be left as it was"
    );

    let ended = end_stopped(pid, stat.start_ticks, Duration::from_secs(1))
        .expect("the recorded process is ended");
    assert!(ended.ended(), "{ended:?}");
    let status = child.0.wait().expect("reap ended shell");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}
