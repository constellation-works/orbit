//! Real held-slot admission through the public agent dispatch boundary.
use super::*;
use std::io::BufRead;
use std::process::{Command, Stdio};

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_BUILD_WAIT_FIXTURE";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let scratch =
        orbit_common::fs::path::ensure_orbit_scratch_dir(env!("CARGO_MANIFEST_DIR")).unwrap();
    let home = tempfile::tempdir_in(scratch).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    child
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut child, test, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

struct Holder(std::process::Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn held_slot(directory: &Path) -> Holder {
    fs::create_dir_all(directory).unwrap();
    let mut holder = Command::new("python3").args(["-c", "import fcntl,sys; f=open(sys.argv[1], 'a+'); fcntl.flock(f,fcntl.LOCK_EX); print('held',flush=True); sys.stdin.read()"])
        .arg(directory.join("slot-001.lock")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "held");
    Holder(holder)
}

#[test]
fn held_slot_wait_extends_deadline_and_is_durable() {
    if !isolated("v2_cli_agent::build_budget::held_slot_wait_extends_deadline_and_is_durable") {
        return;
    }
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let budget = root.path().join("budget");
    let mut holder = held_slot(&budget);
    let wrapper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/build-budget.py")
        .canonicalize()
        .unwrap();
    let queued = root.path().join("queued");
    // The holder stays until the wrapper's own wait clock passes the activity
    // budget. Post-admission work then returns at once, so startup speed does
    // not eat a fixed gap between the hold and the wall-clock timeout.
    let budget_ms = 4_000;
    let script = format!(
        r#"#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys,time
sys.stdin.read()
os.environ['ORBIT_BUILD_BUDGET_DIR']={budget:?}
os.environ['ORBIT_BUILD_SLOTS']='1'
os.environ.pop('ORBIT_BUILD_BUDGET_HELD',None)
p=subprocess.Popen([sys.executable,{wrapper:?},'--',sys.executable,'-c','pass'])
directory=pathlib.Path(os.environ['ORBIT_ACTIVITY_BUILD_BUDGET_DIR'])
deadline=time.monotonic()+30
snapshot=None
while True:
    files=list(directory.glob('*.json'))
    if files:
        try:
            snapshot=json.loads(files[0].read_text())
        except (OSError, json.JSONDecodeError):
            snapshot=None
        else:
            if snapshot.get('elapsed_ms', 0) >= {budget_ms}:
                break
    if time.monotonic()>deadline:
        raise RuntimeError('wait did not pass the budget: %r' % (snapshot,))
    time.sleep(0.01)
pathlib.Path({queued:?}).touch()
assert p.wait()==0
print(json.dumps({{'schemaVersion':1,'status':'success','result':{{}},'error':None}}))
"#,
        budget = budget.to_str().unwrap(),
        wrapper = wrapper.to_str().unwrap(),
        queued = queued.to_str().unwrap(),
        budget_ms = budget_ms,
    );
    let fake = fake_cli("claude", &script).unwrap();
    let release = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !queued.exists() {
            assert!(
                Instant::now() < deadline,
                "fake agent did not hold the slot through the budget"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(holder.0.stdin.take());
        holder.0.wait().unwrap();
    });
    let (writer, store) = build_writer(root.path(), "wait-credit").unwrap();
    let mut spec = cli_agent_loop_spec(None);
    spec.wall_clock_timeout_seconds = budget_ms / 1000;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "wait_credit",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({"prompt":"test", "workspace_path":root.path()}),
        audit: writer,
        run_id: "wait-credit",
        host: Some(&ScriptHost::new(fake.cli_path())),
    })
    .unwrap();
    release.join().unwrap();
    assert!(
        outcome.success,
        "runtime credit must permit work after admission: {outcome:?}"
    );
    let waits = &outcome.output["build_budget_waits"];
    assert_eq!(waits["count"], 1);
    assert!(waits["total_ms"].as_u64().unwrap() >= budget_ms, "{waits}");
    assert_eq!(waits["total_ms"], waits["longest_ms"]);
    // The hold runs until the wrapper's own clock passes the budget, so the
    // recorded queue interval overshoots it. Credit stays capped at the
    // original wall-clock budget (`queued_wall_ms.min(timeout)`).
    let queued_wall_ms = waits["queued_wall_ms"].as_u64().unwrap();
    assert!(
        queued_wall_ms >= budget_ms,
        "the hold lasts through the budget: {waits}"
    );
    assert_eq!(
        waits["deadline_extension_ms"].as_u64().unwrap(),
        queued_wall_ms.min(budget_ms),
        "queue credit is capped at the original wall-clock budget: {waits}"
    );
    assert_eq!(
        outcome.output["effective_deadline_ms"].as_u64().unwrap(),
        budget_ms + waits["deadline_extension_ms"].as_u64().unwrap(),
        "the recorded deadline must include earned queue credit",
    );
    assert!(outcome.output["duration_ms"].as_u64().unwrap() > budget_ms);
    let events = events_snapshot(&store, "wait-credit").unwrap();
    let persisted = events
        .iter()
        .find(|event| event.envelope.event_type == "cli.invocation.build_budget")
        .unwrap();
    assert_eq!(
        serde_json::to_value(&persisted.kind).unwrap()["total_ms"],
        waits["total_ms"]
    );
}

#[test]
fn slot_that_never_frees_times_out_at_twice_the_original_budget() {
    if !isolated(
        "v2_cli_agent::build_budget::slot_that_never_frees_times_out_at_twice_the_original_budget",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let budget = root.path().join("budget");
    let _holder = held_slot(&budget);
    let wrapper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/build-budget.py")
        .canonicalize()
        .unwrap();
    let fake = fake_cli("claude", &format!("#!/bin/sh\ncat > /dev/null\nunset ORBIT_BUILD_BUDGET_HELD\nexport ORBIT_BUILD_BUDGET_DIR='{}' ORBIT_BUILD_SLOTS=1\nexec python3 '{}' -- true\n", budget.display(), wrapper.display())).unwrap();
    let (writer, store) = build_writer(root.path(), "wait-cap").unwrap();
    let mut spec = cli_agent_loop_spec(None);
    spec.wall_clock_timeout_seconds = 2;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "wait_cap",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({"prompt":"test", "workspace_path":root.path()}),
        audit: writer,
        run_id: "wait-cap",
        host: Some(&ScriptHost::new(fake.cli_path())),
    })
    .unwrap();
    assert!(!outcome.success);
    assert_eq!(outcome.output["timed_out"], true);
    let duration = outcome.output["duration_ms"].as_u64().unwrap();
    assert!(
        (3700..=5000).contains(&duration),
        "bounded deadline plus process cleanup: {duration}ms"
    );
    assert_eq!(
        outcome.output["build_budget_waits"]["deadline_extension_ms"],
        2000
    );
    let events = events_snapshot(&store, "wait-cap").unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.envelope.event_type == "cli.invocation.build_budget")
    );
}
