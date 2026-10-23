// The agent JSONL feed keeps every relay line when its writer falls behind.
//
// The operational feed may drop lines once its queue is full. The agent feed
// must not: a stalled worker blocks the emitter, and every sequence emitted
// while that worker is held is still in `orbit-agent.jsonl` after shutdown.
// The gate is a FIFO the writer opens on its first line, so the queue fills
// before any byte is stored. No retries and no weaker count.

const AGENT_FEED_CHILD: &str = "agent_feed_keeps_lines_while_its_writer_is_stalled_child";
const AGENT_FEED_TARGET: &str = "orbit.test.agent_feed";

#[test]
fn agent_feed_keeps_lines_while_its_writer_is_stalled() {
    assert_child_test_exists(AGENT_FEED_CHILD);
    let home = tempfile::tempdir().expect("temp home");
    let gate = home.path().join("agent-write.gate");
    let made = Command::new("mkfifo")
        .arg(&gate)
        .status()
        .expect("spawn mkfifo");
    assert!(made.success(), "create the agent writer gate at {gate:?}");

    let mut command = Command::new(std::env::current_exe().unwrap());
    clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", AGENT_FEED_CHILD, "--ignored", "--nocapture"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("ORBIT_TEST_AGENT_JSONL_GATE", &gate)
        .env_remove("RUST_LOG");
    let output = run_child_test(&mut command, AGENT_FEED_CHILD, home.path());
    assert_child_test_passed(
        AGENT_FEED_CHILD,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

#[test]
#[ignore = "re-executed by agent_feed_keeps_lines_while_its_writer_is_stalled"]
fn agent_feed_keeps_lines_while_its_writer_is_stalled_child() {
    let lines = logging::JSONL_QUEUE_LINES + 2;
    logging::init_subscriber_with_file_filter("error", "info");

    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let producer_tid = Arc::new(AtomicU64::new(0));
    let completed_thread = Arc::clone(&completed);
    let attempted_thread = Arc::clone(&attempted);
    let tid_thread = Arc::clone(&producer_tid);
    let producer = std::thread::spawn(move || {
        let tid = std::fs::read_link("/proc/thread-self")
            .expect("current task")
            .file_name()
            .expect("task id")
            .to_string_lossy()
            .parse::<u64>()
            .expect("task id integer");
        tid_thread.store(tid, Ordering::Release);
        for seq in 0..lines {
            attempted_thread.store(seq + 1, Ordering::Release);
            tracing::info!(
                target: AGENT_FEED_TARGET,
                agent_output = true,
                seq,
                "relay line"
            );
            completed_thread.store(seq + 1, Ordering::Release);
        }
    });

    let started = Instant::now();
    let mut stalled = false;
    let mut same_for = 0u8;
    let mut last_done = None;
    let (done, started_events, state, wchan) = loop {
        let done = completed.load(Ordering::Acquire);
        let started_events = attempted.load(Ordering::Acquire);
        if done == lines {
            break (done, started_events, None, String::new());
        }
        let tid = producer_tid.load(Ordering::Acquire);
        let (state, wchan) = thread_wait(tid);
        let at_cap = (logging::JSONL_QUEUE_LINES..=logging::JSONL_QUEUE_LINES + 1).contains(&done)
            && started_events == done + 1;
        let sleeping = matches!(state, Some('S' | 'D')) && wchan.contains("futex");
        if at_cap && sleeping && last_done == Some(done) {
            same_for += 1;
            if same_for >= 3 {
                stalled = true;
                break (done, started_events, state, wchan);
            }
        } else {
            same_for = 0;
        }
        last_done = Some(done);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "agent feed did not fill or block within 5s: done={done} attempted={started_events} state={state:?} wchan={wchan:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    let gate = std::env::var("ORBIT_TEST_AGENT_JSONL_GATE").expect("agent write gate");
    let release = std::fs::OpenOptions::new()
        .write(true)
        .open(&gate)
        .expect("release the agent writer");
    producer.join().expect("agent feed producer");
    logging::shutdown_jsonl_writer();
    drop(release);

    let operational = logging::global_jsonl_log_path().expect("operational log path");
    let agent = logging::agent_jsonl_log_path(&operational).expect("agent log path");
    let raw = std::fs::read_to_string(&agent).unwrap_or_default();
    let mut seqs = Vec::new();
    for line in raw.lines() {
        let record: Value = serde_json::from_str(line).expect("agent feed line");
        if record["target"] != AGENT_FEED_TARGET {
            continue;
        }
        let seq = record["fields"]["seq"].as_u64().expect("relay sequence") as usize;
        seqs.push(seq);
    }
    seqs.sort_unstable();
    assert!(
        stalled,
        "emitter finished all {lines} events without blocking (feed kept {}); done={done} attempted={started_events} state={state:?} wchan={wchan}. A lossy agent queue drops the overflow instead of waiting",
        seqs.len()
    );
    let expected: Vec<usize> = (0..lines).collect();
    assert_eq!(
        seqs,
        expected,
        "a blocked agent feed must keep every relay line in {}",
        agent.display()
    );
}

fn thread_wait(tid: u64) -> (Option<char>, String) {
    if tid == 0 {
        return (None, String::new());
    }
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).unwrap_or_default();
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next());
    let wchan = std::fs::read_to_string(format!("/proc/self/task/{tid}/wchan")).unwrap_or_default();
    (state, wchan)
}
