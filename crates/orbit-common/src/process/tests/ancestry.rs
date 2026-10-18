//! The macOS process tree names the calling process and a live child, and
//! drops the child once it has been reaped. The session tree keeps a child
//! that launchd adopted.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::process::OwnedChild;
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

    let child = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let mut child = OwnedChild::from_spawned(child);
    let child_pid = child.pid();
    let deadline = Instant::now() + Duration::from_secs(5);
    let found = loop {
        let current = ancestry::process_tree(self_pid);
        if current.is_none() {
            child.kill_and_reap();
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
            child.kill_and_reap();
            panic!("sleep {child_pid} never appeared in the process tree: {current:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        found.iter().any(|process| process.key.pid == child_pid),
        "direct child is part of the tree"
    );

    // TreeCpuAccount drops unpaid debt after the owner's next sighting. That
    // is safe only because a dead but unwaited child stays listed with a
    // stable total until its parent reaps it, so its reap can't arrive after
    // its debt has gone (that would count its CPU twice).
    OwnedChild::signal_spawned_group(child_pid, libc::SIGKILL).expect("kill the sleep");
    // Safety: `info` is a writable siginfo_t; WNOWAIT leaves the child unreaped.
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            child_pid,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(rc, 0, "wait for the sleep to exit without reaping it");
    let zombie_cpu = |label: &str| {
        let tree = ancestry::process_tree(self_pid).expect("tree readable with a zombie child");
        tree.iter()
            .find(|process| process.key.pid == child_pid)
            .unwrap_or_else(|| panic!("an exited, unreaped child is still in the tree ({label})"))
            .cpu_time_ns
    };
    let first = zombie_cpu("first read");
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        zombie_cpu("second read"),
        first,
        "an unreaped child's CPU total is stable"
    );

    child.kill_and_reap();
    let after = ancestry::process_tree(self_pid).expect("tree still readable after reap");
    assert!(
        !contains_pid(&after, child_pid),
        "a reaped child is not still in the tree"
    );
}

/// A background job whose shell exits is adopted by launchd and leaves the
/// parent-link walk, but it is still in the session the shell led, so the
/// session tree keeps it, with no parent.
#[test]
fn an_orphan_stays_in_the_session_tree_its_shell_led() {
    use std::io::{BufRead, BufReader};

    const TEST: &str =
        "process::tests::ancestry::an_orphan_stays_in_the_session_tree_its_shell_led";
    let self_pid = std::process::id();
    let mut tree = ancestry::SessionTree::new(self_pid);
    if tree.snapshot().is_none() {
        match crate::test_env::ps_stat(self_pid) {
            crate::test_env::PsRun::Denied(reason) => {
                crate::test_env::report_ps_denied(TEST, &reason);
                return;
            }
            crate::test_env::PsRun::Ran(output) => panic!(
                "session tree is unreadable but ps ran (status {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }

    let mut shell = Command::new("/bin/sh");
    shell
        .args([
            "-c",
            "/bin/sleep 30 </dev/null >/dev/null 2>&1 & echo $!; read _ || :",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Safety: `setsid` is async-signal-safe and touches only the child.
    unsafe {
        shell.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut shell = shell.spawn().expect("spawn the session's shell");
    // The shell leads its session and its job's process group, so killing
    // the group on the way out also stops the orphaned job.
    struct KillGroup(libc::pid_t);
    impl Drop for KillGroup {
        fn drop(&mut self) {
            // Safety: signals only the group this test created.
            unsafe { libc::killpg(self.0, libc::SIGKILL) };
        }
    }
    let shell_pid = shell.id();
    let _group = KillGroup(shell_pid as libc::pid_t);
    let mut line = String::new();
    BufReader::new(shell.stdout.take().expect("shell stdout"))
        .read_line(&mut line)
        .expect("read the job's pid");
    let job: u32 = line.trim().parse().expect("the job's pid");

    let before = tree.snapshot().expect("session tree readable");
    let listed_job = before
        .iter()
        .find(|process| process.key.pid == job)
        .unwrap_or_else(|| panic!("the job is under its shell: {before:?}"));
    assert_eq!(
        listed_job.parent.map(|parent| parent.pid),
        Some(shell_pid),
        "the job is walked through its shell while the shell lives"
    );

    drop(shell.stdin.take());
    assert!(shell.wait().expect("reap the shell").success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while ancestry::process_start_and_parent(job).map(|(_, parent)| parent) != Some(1) {
        assert!(Instant::now() < deadline, "launchd never adopted the job");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !contains_pid(
            &ancestry::process_tree(self_pid).expect("tree readable"),
            job
        ),
        "the parent-link walk loses the orphan"
    );
    let after = tree.snapshot().expect("session tree readable");
    let adopted = after
        .iter()
        .find(|process| process.key == listed_job.key)
        .unwrap_or_else(|| panic!("the session tree keeps the orphaned job: {after:?}"));
    assert_eq!(adopted.parent, None, "an adopted orphan has no parent");
    // Other tests' children may share this tree, but launchd's own children
    // are in sessions nothing here leads.
    assert!(
        after.len() < before.len() + 16,
        "the session tree adopts orphans, not launchd's children: {after:?}"
    );
}

fn contains_pid(processes: &[ProcessTreeProcess], pid: u32) -> bool {
    processes.iter().any(|process| process.key.pid == pid)
}

/// A child reparented between its listing and measurement must not lose its
/// accounting identity for one sample and charge its lifetime a second time.
#[test]
fn session_cpu_counts_a_child_once_when_orphaned_during_a_walk() {
    use ancestry::{ProcessStartKey, TreeCpuAccount};

    let root = ProcessTreeProcess {
        key: ProcessStartKey {
            pid: 10,
            starttime: 10,
        },
        parent: None,
        cpu_time_ns: 0,
        reaped_cpu_time_ns: 0,
        phys_footprint: 0,
    };
    let child = ProcessTreeProcess {
        key: ProcessStartKey {
            pid: 20,
            starttime: 20,
        },
        parent: Some(root.key),
        cpu_time_ns: 10,
        ..root
    };
    let mut account = TreeCpuAccount::new();
    account.observe(&[root], |_| None);
    account.observe(&[root, child], |_| None);
    // The child was queued under its old parent, then rejected because it
    // was reparented to launchd. It can miss the orphan scan too, but the
    // public identity probe still finds this same process alive.
    account.observe(&[root], |pid| {
        assert_eq!(pid, child.key.pid);
        Some((child.key, 1))
    });
    account.observe(&[root], |_| Some((child.key, 1)));
    assert_eq!(account.cumulative_ns(), 10, "an absent orphan adds no CPU");
    let orphan = ProcessTreeProcess {
        parent: None,
        cpu_time_ns: 20,
        ..child
    };
    account.observe(&[root, orphan], |_| None);
    assert_eq!(
        account.cumulative_ns(),
        20,
        "reparenting during a snapshot must not charge an orphan's previously observed CPU twice"
    );
    account.observe(&[root], |_| Some((child.key, 1)));
    let root = ProcessTreeProcess {
        reaped_cpu_time_ns: 5,
        ..root
    };
    account.observe(&[root], |_| None);
    assert_eq!(
        account.cumulative_ns(),
        25,
        "an orphan reaped outside the tree must not hide the root's unrelated reaped CPU"
    );
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

/// One-time native calibration, deliberately outside recurring gates. Uses
/// the existing binary in an exact child, so other tests cannot charge CPU
/// into RUSAGE_SELF. The one-thread workload targets 250 ms of wall time,
/// checked every 1,000 arithmetic operations; tolerance is 5% plus 5 ms for
/// non-atomic snapshot/syscall boundaries.
#[test]
#[ignore = "bounded native accounting calibration; run explicitly in isolation"]
#[allow(deprecated)] // The same supported Mach API queried by the sampler.
fn native_cpu_time_agrees_with_getrusage() {
    const TEST: &str = "process::tests::ancestry::native_cpu_time_agrees_with_getrusage";
    const CHILD: &str = "ORBIT_NATIVE_CPU_CALIBRATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut command = Command::new(std::env::current_exe().expect("existing test binary"));
        crate::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", TEST, "--ignored", "--nocapture"])
            .env(CHILD, "1");
        let output = command.output().expect("isolated native calibration");
        std::io::stdout()
            .write_all(&output.stdout)
            .expect("calibration child evidence");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    fn cpu_ns() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // Safety: the pointer names a writable rusage structure.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        // Safety: the successful syscall initialized the structure.
        let usage = unsafe { usage.assume_init() };
        ((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64 * 1_000_000
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64)
            * 1_000
    }
    fn tree_cpu_ns() -> u64 {
        ancestry::process_tree(std::process::id())
            .expect("native tree must be readable for calibration")
            .iter()
            .find(|process| process.key.pid == std::process::id())
            .expect("own process")
            .cpu_time_ns
    }
    fn raw_cpu_ticks() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // Safety: the flavor selects the writable structure's layout.
        assert_eq!(
            unsafe {
                libc::proc_pid_rusage(
                    std::process::id() as libc::c_int,
                    libc::RUSAGE_INFO_V4,
                    usage.as_mut_ptr().cast(),
                )
            },
            0
        );
        // Safety: the successful syscall initialized the structure.
        let usage = unsafe { usage.assume_init() };
        usage.ri_user_time + usage.ri_system_time
    }
    let mut timebase = std::mem::MaybeUninit::<libc::mach_timebase_info_data_t>::zeroed();
    // Safety: the pointer names a writable timebase structure.
    assert_eq!(
        unsafe { libc::mach_timebase_info(timebase.as_mut_ptr()) },
        libc::KERN_SUCCESS
    );
    // Safety: the successful syscall initialized the structure.
    let timebase = unsafe { timebase.assume_init() };
    let reference_before = cpu_ns();
    let raw_before = raw_cpu_ticks();
    let before = tree_cpu_ns();
    let started = Instant::now();
    let mut value = 1_u64;
    while started.elapsed() < Duration::from_millis(250) {
        for _ in 0..1_000 {
            value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }
    let workload_wall_ns = started.elapsed().as_nanos();
    let measured = tree_cpu_ns() - before;
    let raw = raw_cpu_ticks() - raw_before;
    let reference = cpu_ns() - reference_before;
    let tolerance = reference / 20 + 5_000_000;
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({
            "schema_version": 1, "numer": timebase.numer, "denom": timebase.denom,
            "raw_own_counter_delta": raw, "incorrect_raw_as_ns": raw,
            "cpu_time_ns_delta": measured, "getrusage_cpu_ns_delta": reference,
            "tolerance_ns": tolerance, "absolute_error_ns": measured.abs_diff(reference),
        "target_wall_ms": 250, "workload_wall_ns": workload_wall_ns, "own_cpu_only": true,
        })
    )
    .expect("native calibration evidence");
    assert!(
        reference >= 100_000_000,
        "insufficient CPU work for a meaningful calibration: {reference}"
    );
    assert!(
        measured.abs_diff(reference) <= tolerance,
        "tree {measured} ns vs getrusage {reference} ns; tolerance {tolerance} ns"
    );
}

/// One-time native check, outside recurring gates, that the tree account
/// counts descendants that were reaped between samples, and counts once a
/// child that samples saw alive before it was reaped. An isolated sampler
/// process samples its own tree every 250 ms around one child per scenario:
/// a spawner whose 24 sequential grandchildren each burn about 100 ms and
/// exit inside one interval, and a 1 s burner seen alive at least twice.
/// After the reap, the tree CPU beyond the sampler's own must be within 10%
/// plus 20 ms of `getrusage(RUSAGE_CHILDREN)` over the same scenario.
#[test]
#[ignore = "bounded native reaped-descendant accounting; run explicitly in isolation"]
fn native_tree_cpu_counts_reaped_descendants_once() {
    const TEST: &str = "process::tests::ancestry::native_tree_cpu_counts_reaped_descendants_once";
    const ROLE: &str = "ORBIT_NATIVE_TREE_CPU_ROLE";
    const BURN_MS: &str = "ORBIT_NATIVE_TREE_CPU_BURN_MS";
    const GRANDCHILDREN: u32 = 24;
    const INTERVAL: Duration = Duration::from_millis(250);

    fn role(name: &str, burn_ms: u64) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("existing test binary"));
        command
            .args(["--exact", TEST, "--ignored", "--quiet"])
            .env(ROLE, name)
            .env(BURN_MS, burn_ms.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
    }
    fn burn(wall: Duration) {
        let started = Instant::now();
        let mut value = 1_u64;
        while started.elapsed() < wall {
            for _ in 0..1_000 {
                value =
                    std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        }
    }
    fn children_cpu_ns() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // Safety: the pointer names a writable rusage structure.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) },
            0
        );
        // Safety: the successful syscall initialized the structure.
        let usage = unsafe { usage.assume_init() };
        ((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64 * 1_000_000
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64)
            * 1_000
    }
    fn snapshot() -> Vec<ProcessTreeProcess> {
        ancestry::process_tree(std::process::id())
            .expect("native tree must be readable for this check")
    }
    fn own_ns(tree: &[ProcessTreeProcess]) -> u64 {
        tree.iter()
            .find(|process| process.key.pid == std::process::id())
            .expect("own process")
            .cpu_time_ns
    }

    let burn_ms = std::env::var(BURN_MS)
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(0);
    match std::env::var(ROLE).as_deref() {
        Ok("burn") => {
            burn(Duration::from_millis(burn_ms));
            return;
        }
        Ok("spawner") => {
            for _ in 0..GRANDCHILDREN {
                let status = role("burn", burn_ms).status().expect("spawn grandchild");
                assert!(status.success(), "grandchild {status}");
            }
            return;
        }
        Ok("sampler") => {}
        _ => {
            let mut command = role("sampler", 0);
            crate::test_env::clear_inherited_authority(|key| {
                command.env_remove(key);
            });
            command.args(["--nocapture"]).stdout(Stdio::piped());
            let output = command.output().expect("isolated tree CPU sampler");
            std::io::stdout()
                .write_all(&output.stdout)
                .expect("tree CPU evidence");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
    }

    for (scenario, child_role, child_burn_ms, min_alive_samples) in [
        ("short-lived grandchildren", "spawner", 100, 0),
        ("long-lived child", "burn", 1_000, 2),
    ] {
        let mut account = ancestry::TreeCpuAccount::new();
        let reference_before = children_cpu_ns();
        let first = snapshot();
        let own_before = own_ns(&first);
        account.observe(&first, ancestry::process_start_and_parent);
        let mut child = OwnedChild::from_spawned(
            role(child_role, child_burn_ms)
                .process_group(0)
                .spawn()
                .expect("spawn scenario child"),
        );
        let child_pid = child.pid();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut samples = 0_u32;
        let mut alive_samples = 0_u32;
        let status = loop {
            std::thread::sleep(INTERVAL);
            if let Some(status) = child.child_mut().try_wait().expect("poll scenario child") {
                // Each grandchild was waited for, so the group is empty.
                child.mark_reaped();
                break status;
            }
            if Instant::now() > deadline {
                child.kill_and_reap();
                panic!("{scenario}: child did not finish within 60 s");
            }
            let tree = snapshot();
            alive_samples += u32::from(contains_pid(&tree, child_pid));
            samples += 1;
            account.observe(&tree, ancestry::process_start_and_parent);
        };
        assert!(status.success(), "{scenario}: child {status}");
        // The reap moved the child's whole total into this process's own
        // reaped counter; the last snapshot is what charges it.
        let last = snapshot();
        let own_after = own_ns(&last);
        account.observe(&last, ancestry::process_start_and_parent);
        let measured = account
            .cumulative_ns()
            .saturating_sub(own_after - own_before);
        let reference = children_cpu_ns() - reference_before;
        let tolerance = reference / 10 + 20_000_000;
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({
                "schema_version": 1, "scenario": scenario, "samples": samples,
                "alive_samples": alive_samples, "tree_children_cpu_ns": measured,
                "rusage_children_cpu_ns": reference, "tolerance_ns": tolerance,
                "absolute_error_ns": measured.abs_diff(reference),
            })
        )
        .expect("tree CPU evidence");
        assert!(
            alive_samples >= min_alive_samples,
            "{scenario}: child sampled alive {alive_samples} times, need {min_alive_samples}"
        );
        assert!(
            reference >= 900_000_000,
            "{scenario}: insufficient child CPU for a meaningful check: {reference}"
        );
        assert!(
            measured.abs_diff(reference) <= tolerance,
            "{scenario}: tree {measured} ns vs RUSAGE_CHILDREN {reference} ns; tolerance {tolerance} ns"
        );
    }
}

/// Kernel-call interleavings must never attribute a replacement process's
/// usage or descendants to the tree the walk started from.
#[test]
fn tree_walk_omits_reused_pids_and_reparented_children() {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use crate::process::ancestry::ProcessStartKey;
    use crate::process::ancestry::darwin_tree::{
        ReadError, mach_cpu_ns, read_process_with, snapshot_with,
    };

    // Pure arithmetic and injected platform failure outcomes share this
    // existing kernel interleaving matrix; no host load is needed.
    for (user, system, timebase, expected) in [
        (9, 3, Some((1, 1)), Some(12)),
        (12_000_990, 0, Some((125, 3)), Some(500_041_250)),
        (1, 1, Some((3, 2)), Some(3)),
        (u64::MAX, u64::MAX, Some((1, 2)), Some(u64::MAX)),
        (u64::MAX, 0, Some((u32::MAX, u32::MAX)), Some(u64::MAX)),
        (u64::MAX, 1, Some((1, 1)), None),
        (u64::MAX, 0, Some((125, 3)), None),
        (0, 0, Some((1, 1)), Some(0)),
        (0, 0, None, None),
        (1, 0, Some((0, 3)), None),
        (1, 0, Some((125, 0)), None),
    ] {
        match (mach_cpu_ns(user, system, timebase), expected) {
            (Ok(actual), Some(expected)) => assert_eq!(actual, expected),
            (Err(ReadError::Unreadable), None) => {}
            (actual, expected) => {
                panic!("conversion {user}+{system} {timebase:?}: {actual:?}, expected {expected:?}")
            }
        }
    }

    // Usage is charged to one identity only if the start key and parent read
    // around it agree. Reuse or reparenting reads as an exit; a refused read
    // fails closed.
    for (name, second_start, rusage_denied, expected) in [
        ("stable", Ok((20, 1)), false, Ok(1)),
        (
            "pid reused during usage read",
            Ok((21, 1)),
            false,
            Err("gone"),
        ),
        (
            "reparented during usage read",
            Ok((20, 9)),
            false,
            Err("gone"),
        ),
        ("identity reread denied", Err(()), false, Err("unreadable")),
        ("usage read denied", Ok((20, 1)), true, Err("unreadable")),
    ] {
        let reads = Cell::new(0);
        let measured = read_process_with(
            2,
            |_| {
                reads.set(reads.get() + 1);
                match (reads.get(), second_start) {
                    (1, _) => Ok((20, 1)),
                    (_, Ok(start)) => Ok(start),
                    (_, Err(())) => Err(ReadError::Unreadable),
                }
            },
            |_| {
                if rusage_denied {
                    Err(ReadError::Unreadable)
                } else {
                    Ok((900_000_000, 300_000_000, 4096))
                }
            },
        );
        match (measured, expected) {
            (Ok((process, parent)), Ok(expected_parent)) => {
                assert_eq!(
                    process.key,
                    ProcessStartKey {
                        pid: 2,
                        starttime: 20
                    },
                    "{name}"
                );
                assert_eq!(process.cpu_time_ns, 900_000_000, "{name}");
                assert_eq!(process.reaped_cpu_time_ns, 300_000_000, "{name}");
                assert_eq!(parent, expected_parent, "{name}");
            }
            (Err(ReadError::Gone), Err("gone"))
            | (Err(ReadError::Unreadable), Err("unreadable")) => {}
            (measured, _) => panic!("{name}: {measured:?}"),
        }
    }

    // Tree R(1) -> A(2) -> B(3) -> C(4), each start key `pid * 10`.
    // Every fault lands at one kernel-call boundary of the walk.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Fault {
        None,
        RootGone,
        ChildGone,
        ChildReparented,
        ChildListingGone,
        RootReplacedDuringListing,
        ParentReplacedBeforeChildRead,
        ParentReparentedBeforeChildRead,
        ChildReplacedDuringListing,
        ParentReadDenied,
    }
    for (fault, expected) in [
        (Fault::None, Some(vec![1, 2, 3, 4])),
        (Fault::RootGone, None),
        (Fault::ChildGone, Some(vec![1])),
        (Fault::ChildReparented, Some(vec![1])),
        (Fault::ChildListingGone, Some(vec![1, 2])),
        (Fault::RootReplacedDuringListing, None),
        (Fault::ParentReplacedBeforeChildRead, Some(vec![1])),
        (Fault::ParentReparentedBeforeChildRead, Some(vec![1, 2, 3])),
        (Fault::ChildReplacedDuringListing, Some(vec![1, 2])),
        (Fault::ParentReadDenied, None),
    ] {
        let listed = RefCell::new(Vec::new());
        let start_reads = RefCell::new(HashMap::<u32, u32>::new());
        let parent_reparented = Cell::new(false);
        let original = |pid: u32| (u64::from(pid) * 10, pid.saturating_sub(1));
        let tree = snapshot_with(
            1,
            |pid| {
                // B has listed C and passed its post-listing identity check.
                // A now exits, reparenting B outside R; C still names B.
                if fault == Fault::ParentReparentedBeforeChildRead && pid == 4 {
                    parent_reparented.set(true);
                }
                if fault == Fault::RootGone && pid == 1 || fault == Fault::ChildGone && pid == 2 {
                    return Err(ReadError::Gone);
                }
                let (starttime, mut parent) = original(pid);
                if fault == Fault::ChildReparented && pid == 2 {
                    parent = 9;
                }
                Ok((
                    ProcessTreeProcess {
                        key: ProcessStartKey { pid, starttime },
                        parent: None,
                        cpu_time_ns: u64::from(pid) * 1_000,
                        reaped_cpu_time_ns: 0,
                        phys_footprint: u64::from(pid) * 4_096,
                    },
                    parent,
                ))
            },
            |pid| {
                listed.borrow_mut().push(pid);
                match pid {
                    2 if fault == Fault::ChildListingGone => Err(ReadError::Gone),
                    1..=3 => Ok(vec![pid + 1]),
                    _ => Ok(Vec::new()),
                }
            },
            |pid| {
                let reads = {
                    let mut counts = start_reads.borrow_mut();
                    let count = counts.entry(pid).or_default();
                    *count += 1;
                    *count
                };
                let after_listing = listed.borrow().contains(&pid);
                let (starttime, parent) = original(pid);
                let replaced = Ok((starttime + 1, parent));
                match (fault, pid) {
                    (Fault::RootReplacedDuringListing, 1) if after_listing => replaced,
                    (Fault::ChildReplacedDuringListing, 2) if after_listing => replaced,
                    // The root's first reread follows its own listing; the
                    // second is the ownership check for its queued child.
                    (Fault::ParentReplacedBeforeChildRead, 1) if reads > 1 => replaced,
                    (Fault::ParentReadDenied, 1) if reads > 1 => Err(ReadError::Unreadable),
                    (Fault::ParentReparentedBeforeChildRead, 3) if parent_reparented.get() => {
                        Ok((starttime, 9))
                    }
                    _ => Ok((starttime, parent)),
                }
            },
        );
        if let Some(tree) = &tree {
            for process in tree {
                // Each member names the identity of the member that listed it.
                let pid = process.key.pid;
                let listed_by = (pid > 1).then(|| ProcessStartKey {
                    pid: pid - 1,
                    starttime: u64::from(pid - 1) * 10,
                });
                assert_eq!(process.parent, listed_by, "{fault:?}");
            }
        }
        let pids = tree.map(|tree| {
            tree.iter()
                .map(|process| process.key.pid)
                .collect::<Vec<_>>()
        });
        assert_eq!(pids, expected, "{fault:?}");
    }
}
