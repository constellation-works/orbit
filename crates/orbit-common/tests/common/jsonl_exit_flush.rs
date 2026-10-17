//  The JSONL feed keeps the last records a process logs before it exits.
// 
//  The feed is written by a background worker that opens the file on its first
//  record, so a warning logged right before exit is still queued when the
//  process ends. The child is a fresh process that installs the global
//  subscriber, logs one warning, and exits through `std::process::exit`.
// 
//  The feed opens its file through the private append, so a symlink planted at
//  the feed path is refused rather than followed.

const JSONL_EXIT_CHILD: &str = "warn_then_exit_child";
const EXIT_CODE: i32 = 7;

/// Run the child with `home` as its home, so its feed is the one under it.
fn warn_then_exit_in(home: &Path) {
    assert_child_test_exists(JSONL_EXIT_CHILD);
    let mut command = Command::new(std::env::current_exe().unwrap());
    clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", JSONL_EXIT_CHILD, "--ignored", "--nocapture"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("RUST_LOG", "warn")
        .output()
        .expect("run child");
    assert_eq!(
        output.status.code(),
        Some(EXIT_CODE),
        "child must leave through process exit, not a test result:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn final_record_before_process_exit_reaches_the_jsonl_feed() {
    let home = tempfile::tempdir().expect("temp home");
    warn_then_exit_in(home.path());

    let feed = home.path().join(".orbit/state/logs/orbit.jsonl");
    let raw = std::fs::read_to_string(&feed).unwrap_or_default();
    let records: Vec<Value> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["target"] == "orbit.test.exit")
        .collect();
    assert_eq!(
        records.len(),
        1,
        "the warning logged right before exit must reach {feed:?}: {raw:?}"
    );
    assert_eq!(records[0]["level"], "WARN");
    assert_eq!(records[0]["fields"]["code"], EXIT_CODE);
}

#[cfg(unix)]
#[test]
fn private_append_rejects_a_final_symlink() {
    let home = tempfile::tempdir().expect("temp home");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_file = outside.path().join("outside.log");
    std::fs::write(&outside_file, b"unchanged").expect("outside file");
    let logs = home.path().join(".orbit/state/logs");
    std::fs::create_dir_all(&logs).expect("feed directory");
    let link = logs.join("orbit.jsonl");
    std::os::unix::fs::symlink(&outside_file, &link).expect("symlink");

    warn_then_exit_in(home.path());

    assert_eq!(
        std::fs::read(&outside_file).expect("outside file remains"),
        b"unchanged"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("feed path")
            .file_type()
            .is_symlink(),
        "the planted link is neither followed nor replaced"
    );
}

#[test]
#[ignore = "re-executed by the feed tests above"]
fn warn_then_exit_child() {
    logging::init_default_subscriber("warn");
    tracing::warn!(target: "orbit.test.exit", code = EXIT_CODE, "last words before exit");
    logging::exit(EXIT_CODE);
}

/// Sparse relay archives model 400 MiB/day without writing gigabytes. The
/// operational age window must survive pruning the independent relay budget.
#[test]
fn relay_volume_does_not_evict_seven_days_of_operational_history() {
    use orbit_common::observability::log_rotation::{LogRotationConfig, rotate_and_prune};
    use std::{
        fs::File,
        time::{Duration, SystemTime},
    };
    let logs = tempfile::tempdir().unwrap();
    let operational = logs.path().join("orbit.jsonl");
    let agent = logging::agent_jsonl_log_path(&operational).unwrap();
    let config = LogRotationConfig::default();
    let now = SystemTime::now();
    let mut retained = Vec::new();
    for day in 1..=8 {
        let modified = now - Duration::from_secs(day * 86_400 - 3600);
        let path = operational.with_file_name(format!("orbit.jsonl.day{day}"));
        let file = File::create(&path).unwrap();
        file.set_len(1024 * 1024).unwrap();
        file.set_modified(modified).unwrap();
        if day <= 7 {
            retained.push(path);
        }
        for part in 0..8 {
            let file =
                File::create(agent.with_file_name(format!("orbit-agent.jsonl.day{day}.{part}")))
                    .unwrap();
            file.set_len(50 * 1024 * 1024).unwrap();
            file.set_modified(modified).unwrap();
        }
    }
    rotate_and_prune(&agent, &config.agent_output());
    rotate_and_prune(&operational, &config);
    assert!(
        retained.iter().all(|path| path.exists()),
        "relay pruning must preserve the operational seven-day window"
    );
    assert!(
        !operational.with_file_name("orbit.jsonl.day8").exists(),
        "operational age pruning still applies"
    );
    let relay_bytes: u64 = std::fs::read_dir(logs.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("orbit-agent.jsonl.")
        })
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert!(relay_bytes <= config.agent_output().max_total_bytes);
}
