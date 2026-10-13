//! The macOS process tree names the calling process and a live child, and
//! drops the child once it has been reaped.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::process::ancestry::{self, ProcessTreeProcess};

#[test]
fn a_child_is_in_the_process_tree_until_it_exits() {
    const TEST: &str = "process::tests::ancestry::a_child_is_in_the_process_tree_until_it_exits";
    let Some(tree) = ancestry::process_tree(std::process::id()) else {
        match crate::test_env::ps_stat(std::process::id()) {
            crate::test_env::PsRun::Denied(reason) => {
                crate::test_env::report_ps_denied(TEST, &reason);
                return;
            }
            crate::test_env::PsRun::Ran(output) => panic!(
                "process tree is unreadable but ps ran (status {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    };
    let self_pid = std::process::id();
    let mine = tree
        .iter()
        .find(|process| process.key.pid == self_pid)
        .expect("the tree includes its root");
    assert!(
        mine.phys_footprint > 0,
        "ri_phys_footprint of this process is non-zero"
    );
    // A second read cannot match the first byte for byte: the walk allocates
    // between them. It still has to be the footprint, which stays at or under
    // the lifetime maximum, and nearer that counter than resident size when
    // the two differ.
    let direct = direct_rusage(self_pid);
    assert!(
        mine.phys_footprint <= direct.lifetime_max,
        "tree footprint {} exceeds ri_lifetime_max_phys_footprint {}",
        mine.phys_footprint,
        direct.lifetime_max
    );
    if direct.phys_footprint.abs_diff(direct.resident_size) > 1024 * 1024 {
        let nearer_footprint = mine.phys_footprint.abs_diff(direct.phys_footprint)
            <= mine.phys_footprint.abs_diff(direct.resident_size);
        assert!(
            nearer_footprint,
            "tree reports resident size rather than ri_phys_footprint: tree {} footprint {} resident {}",
            mine.phys_footprint, direct.phys_footprint, direct.resident_size
        );
    }

    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let child_pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let found = loop {
        let current = ancestry::process_tree(self_pid);
        if current.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            match crate::test_env::ps_stat(self_pid) {
                crate::test_env::PsRun::Denied(reason) => {
                    crate::test_env::report_ps_denied(TEST, &reason);
                    return;
                }
                crate::test_env::PsRun::Ran(_) => {
                    panic!("tree became unreadable after spawning a child, and ps was allowed")
                }
            }
        }
        if current
            .as_ref()
            .is_some_and(|processes| contains_pid(processes, child_pid))
        {
            break current.expect("just checked");
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sleep {child_pid} never appeared in the process tree: {current:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        found.iter().any(|process| process.key.pid == child_pid),
        "direct child is part of the tree"
    );

    child.kill().expect("kill sleep");
    child.wait().expect("reap sleep");
    let after = ancestry::process_tree(self_pid).expect("tree still readable after reap");
    assert!(
        !contains_pid(&after, child_pid),
        "a reaped child is not still in the tree"
    );
}

fn contains_pid(processes: &[ProcessTreeProcess], pid: u32) -> bool {
    processes.iter().any(|process| process.key.pid == pid)
}

struct DirectRusage {
    phys_footprint: u64,
    resident_size: u64,
    lifetime_max: u64,
}

fn direct_rusage(pid: u32) -> DirectRusage {
    use std::mem::MaybeUninit;

    let mut info = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // Safety: `info` is a writable `rusage_info_v4`. The flavor selects that
    // layout; a non-zero return leaves the value unread.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    assert_eq!(rc, 0, "proc_pid_rusage of this process");
    // Safety: a zero return wrote the structure.
    let info = unsafe { info.assume_init() };
    DirectRusage {
        phys_footprint: info.ri_phys_footprint,
        resident_size: info.ri_resident_size,
        lifetime_max: info.ri_lifetime_max_phys_footprint,
    }
}
