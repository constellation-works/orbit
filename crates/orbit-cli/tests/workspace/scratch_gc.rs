//! Age-based `.orbit/tmp` pruning by the worktree GC job, through the real
//! CLI with disposable workspace state.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_types::workflow::PipelineState;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::fixture_crew;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).unwrap();
        crate::git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "scratch-gc"])
            .assert()
            .success();
        // The job validates the default crew before it runs, and the seeded
        // one is disabled on a host without its agent CLI (a hosted runner).
        fixture_crew::configure_sol(&fixture.home.join(".orbit"));
        fixture
    }

    fn orbit(&self) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("RUST_LOG", "off");
        command.timeout(std::time::Duration::from_secs(30));
        command
    }

    fn tmp(&self) -> PathBuf {
        self.work.join(".orbit/tmp")
    }

    fn runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.home.join(".orbit"), &self.work.join(".orbit")).unwrap()
    }

    fn db(&self) -> Connection {
        Connection::open(self.home.join(".orbit/orbit.db")).unwrap()
    }

    fn workspace_id(&self) -> String {
        let config: Value =
            serde_yaml::from_slice(&fs::read(self.work.join(".orbit/config.yaml")).unwrap())
                .unwrap();
        config["workspace_id"].as_str().unwrap().to_owned()
    }

    fn seed_retrying_run(&self) {
        let shown = self
            .orbit()
            .args(["workspace", "show", "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let routing: Value = serde_json::from_slice(&shown).unwrap();
        assert_eq!(routing["registered"], true);
        assert_eq!(
            routing["checkout"]["repo_root"],
            self.work.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            routing["checkout"]["orbit_dir"],
            self.work
                .join(".orbit")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
        let runtime = self.runtime();
        let input = json!({"artifact": ".orbit/tmp/from-input"});
        // Persist a sleeping attempt directly: no worker or timing race is
        // needed. The live fixture PID also prevents unrelated reconciliation.
        self.db().execute(
            "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,created_at,pid) VALUES (?1,?2,'fixture',1,'retrying',?3,'2020-01-01T00:00:00Z','2020-01-01T00:00:00Z',?4)",
            params!["jrun-scratch-retrying", self.workspace_id(), input.to_string(), std::process::id()],
        ).unwrap();
        let mut state = PipelineState::new("jrun-scratch-retrying".into(), "fixture".into(), input);
        state.record_pipeline_output("saved", json!({"artifact": self.tmp().join("from-state")}));
        runtime
            .write_run_state("jrun-scratch-retrying", &state)
            .unwrap();
    }

    /// Run the shipped worktree GC job and return the reap step's output.
    fn gc(&self, inputs: &[&str]) -> Value {
        let mut command = self.orbit();
        command
            .env("ORBIT_OPERATOR", "1")
            .args(["run", "job", "worktree_gc_pipeline", "--json"]);
        // Retain the shipped default's real CLI --wait/output contract. The
        // override matrix needs the same actual detached jobs, but can observe
        // them through the public wait API at its admitted one-second cadence.
        if inputs.is_empty() {
            command.arg("--wait");
        }
        for input in inputs {
            command.args(["--input", input]);
        }
        let output = command.assert().success().get_output().stdout.clone();
        let run: Value = serde_json::from_slice(&output).unwrap();
        if inputs.is_empty() {
            return run["pipeline"]["reap"].clone();
        }
        let runtime = orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
            orbit_core::runtime::OrbitRuntimeRoots {
                global_root: self.home.join(".orbit"),
                shared_root: self.work.join(".orbit"),
                local_root: self.work.join(".orbit"),
            },
        )
        .expect("registered scratch GC runtime");
        let waited = runtime
            .wait_pipeline_runs(
                &[run["run_id"]
                    .as_str()
                    .expect("submitted GC run")
                    .to_string()],
                30,
                1,
                None,
            )
            .expect("wait for the actual submitted scratch GC job");
        let entry = waited
            .results
            .into_iter()
            .next()
            .expect("scratch GC wait entry");
        assert_eq!(
            entry.status,
            orbit_core::PipelineWaitStatus::Success,
            "scratch GC job failed: {entry:?}"
        );
        entry.pipeline.expect("scratch GC pipeline")["reap"].clone()
    }
}

/// Set a path's own mtime `hours` into the past, without following a link.
fn age(path: &Path, hours: u64) {
    let when = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - hours * 3600;
    let stamp = libc::timespec {
        tv_sec: when as libc::time_t,
        tv_nsec: 0,
    };
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is NUL-terminated and both timespecs are initialized.
    let status = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            name.as_ptr(),
            [stamp, stamp].as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(status, 0, "utimensat {}", path.display());
}

fn entry<'a>(scratch: &'a Value, tmp: &Path, name: &str) -> &'a Value {
    let path = tmp.canonicalize().unwrap().join(name);
    scratch["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == path.to_str().unwrap())
        .unwrap_or_else(|| panic!("no report for {name}: {scratch}"))
}

fn prunes_entries_older_than_the_window_and_never_follows_symlinks() {
    let fixture = Fixture::new();
    let tmp = fixture.tmp();
    fs::create_dir_all(&tmp).unwrap();
    let outside = fixture._temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep.txt"), b"fresh target").unwrap();

    fs::write(tmp.join("stale-file"), b"12345").unwrap();
    fs::write(tmp.join("fresh-file"), b"fresh").unwrap();
    fs::write(tmp.join("inside-window"), b"x").unwrap();
    fs::write(tmp.join("just-outside"), b"xyz").unwrap();
    fs::create_dir_all(tmp.join("stale-tree/a")).unwrap();
    fs::write(tmp.join("stale-tree/a/b.txt"), b"1234567").unwrap();
    // One recent file deep inside keeps the whole entry.
    fs::create_dir_all(tmp.join("mixed-tree/sub")).unwrap();
    fs::write(tmp.join("mixed-tree/old.txt"), b"old").unwrap();
    fs::write(tmp.join("mixed-tree/sub/recent.txt"), b"recent").unwrap();
    // Links whose targets are fresh and outside scratch: they age by their own
    // mtime, and removing them must leave the targets alone.
    symlink(&outside, tmp.join("stale-link")).unwrap();
    fs::create_dir(tmp.join("linked-tree")).unwrap();
    symlink(&outside, tmp.join("linked-tree/escape")).unwrap();

    age(&tmp.join("stale-file"), 48);
    age(&tmp.join("inside-window"), 23);
    age(&tmp.join("just-outside"), 25);
    age(&tmp.join("stale-tree/a/b.txt"), 48);
    age(&tmp.join("stale-tree/a"), 48);
    age(&tmp.join("stale-tree"), 48);
    age(&tmp.join("mixed-tree/old.txt"), 48);
    age(&tmp.join("mixed-tree/sub/recent.txt"), 1);
    age(&tmp.join("mixed-tree/sub"), 48);
    age(&tmp.join("mixed-tree"), 48);
    age(&tmp.join("stale-link"), 48);
    age(&tmp.join("linked-tree/escape"), 48);
    age(&tmp.join("linked-tree"), 48);

    // A wider window than every entry's age removes nothing.
    let wide = fixture.gc(&["scratch_older_than_hours=100"]);
    assert_eq!(wide["scratch"]["entries_removed"], 0, "{wide}");
    assert_eq!(wide["scratch"]["entries_kept"], 8, "{wide}");

    // The shipped default is 24 hours.
    let reap = fixture.gc(&[]);
    let scratch = &reap["scratch"];
    assert_eq!(scratch["retention_hours"], 24, "{reap}");
    assert_eq!(scratch["entries_removed"], 5, "{reap}");
    assert_eq!(scratch["entries_skipped"], 0, "{reap}");
    assert_eq!(scratch["entries_kept"], 3, "{reap}");
    for name in [
        "stale-file",
        "stale-tree",
        "stale-link",
        "linked-tree",
        "just-outside",
    ] {
        assert_eq!(entry(scratch, &tmp, name)["action"], "removed", "{name}");
        assert!(fs::symlink_metadata(tmp.join(name)).is_err(), "{name}");
    }
    assert_eq!(entry(scratch, &tmp, "stale-file")["bytes_reclaimed"], 5);
    assert_eq!(entry(scratch, &tmp, "stale-tree")["bytes_reclaimed"], 7);
    assert_eq!(entry(scratch, &tmp, "just-outside")["bytes_reclaimed"], 3);
    let reclaimed: u64 = scratch["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["bytes_reclaimed"].as_u64().unwrap())
        .sum();
    assert_eq!(scratch["bytes_reclaimed"], reclaimed);
    for name in ["fresh-file", "inside-window", "mixed-tree"] {
        assert!(tmp.join(name).exists(), "{name} is inside the window");
    }
    assert_eq!(
        fs::read(outside.join("keep.txt")).unwrap(),
        b"fresh target",
        "a symlink target outside scratch was followed"
    );

    // The override reaches the same entries: zero hours removes the rest.
    let zero = fixture.gc(&["scratch_older_than_hours=0"]);
    assert_eq!(zero["scratch"]["retention_hours"], 0, "{zero}");
    assert_eq!(zero["scratch"]["entries_removed"], 3, "{zero}");
    assert!(tmp.is_dir());
    assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
}

fn a_negative_window_is_refused_before_anything_is_removed() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.tmp()).unwrap();
    fs::write(fixture.tmp().join("stale"), b"x").unwrap();
    age(&fixture.tmp().join("stale"), 48);
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args([
            "run",
            "job",
            "worktree_gc_pipeline",
            "--wait",
            "--input",
            "scratch_older_than_hours=-1",
        ])
        .assert()
        .failure();
    assert!(fixture.tmp().join("stale").exists());
}

#[test]
fn retrying_runs_preserve_input_and_saved_state_references() {
    if crate::isolated_test::run_in_child(
        "scratch_gc::retrying_runs_preserve_input_and_saved_state_references",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture.seed_retrying_run();
    fs::create_dir_all(fixture.tmp()).unwrap();
    for name in ["from-input", "from-state"] {
        // fs::write closes each entry before GC observes it.
        fs::write(fixture.tmp().join(name), name).unwrap();
        age(&fixture.tmp().join(name), 48);
    }
    for inputs in [&[][..], &["scratch_older_than_hours=0"][..]] {
        fs::write(fixture.tmp().join("unrelated"), b"idle").unwrap();
        age(&fixture.tmp().join("unrelated"), 48);
        let reap = fixture.gc(inputs);
        let scratch = &reap["scratch"];
        for name in ["from-input", "from-state"] {
            assert_eq!(fs::read(fixture.tmp().join(name)).unwrap(), name.as_bytes());
            assert_eq!(entry(scratch, &fixture.tmp(), name)["action"], "skipped");
        }
        assert_eq!(scratch["entries_skipped"], 2, "{reap}");
        assert_eq!(scratch["entries_removed"], 1, "{reap}");
        assert!(!fixture.tmp().join("unrelated").exists());
        let state: String = fixture
            .db()
            .query_row(
                "SELECT state FROM job_runs WHERE run_id = 'jrun-scratch-retrying'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "retrying",
            "GC must preserve sleeping attempts without reconciling them"
        );
    }
}

#[test]
fn unreadable_retrying_inventory_or_state_prevents_all_scratch_deletion() {
    if crate::isolated_test::run_in_child(
        "scratch_gc::unreadable_retrying_inventory_or_state_prevents_all_scratch_deletion",
    ) {
        return;
    }
    for corrupt_state in [false, true] {
        let fixture = Fixture::new();
        fixture.seed_retrying_run();
        fs::create_dir_all(fixture.tmp()).unwrap();
        for name in ["from-input", "from-state", "unrelated"] {
            fs::write(fixture.tmp().join(name), name).unwrap();
            age(&fixture.tmp().join(name), 48);
        }
        let runtime = fixture.runtime();
        let sql = if corrupt_state {
            "UPDATE job_run_states SET pipeline_state_json = '{' WHERE run_id = 'jrun-scratch-retrying'"
        } else {
            "UPDATE job_runs SET input_json = '{' WHERE run_id = 'jrun-scratch-retrying'"
        };
        assert_eq!(fixture.db().execute(sql, []).unwrap(), 1);
        // Use the public GC host boundary so corrupt inventory cannot be
        // rejected earlier by the CLI's own run submission or observation.
        let error = RuntimeHost::gc_scratch(&runtime, 0).unwrap_err();
        assert!(matches!(error, orbit_core::OrbitError::Store(_)), "{error}");
        for name in ["from-input", "from-state", "unrelated"] {
            assert_eq!(fs::read(fixture.tmp().join(name)).unwrap(), name.as_bytes());
        }
    }
}

#[cfg(target_os = "linux")]
mod held {
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    struct Process(Child);

    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn an_entry_a_live_process_has_open_or_uses_as_its_cwd_is_skipped_and_reported() {
        let fixture = Fixture::new();
        let tmp = fixture.tmp();
        fs::create_dir_all(tmp.join("held-cwd")).unwrap();
        fs::create_dir_all(tmp.join("held-open")).unwrap();
        fs::write(tmp.join("held-open/data"), b"data").unwrap();
        fs::write(tmp.join("idle"), b"idle").unwrap();

        let in_cwd = Process(
            Command::new("sleep")
                .arg("300")
                .current_dir(tmp.join("held-cwd"))
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        // `exec` replaces the shell, so fd 3 belongs to `sleep` itself.
        let with_open_file = Process(
            Command::new("sh")
                .current_dir(&fixture.work)
                .args(["-c", "exec sleep 300 3< .orbit/tmp/held-open/data"])
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let fd = PathBuf::from(format!("/proc/{}/fd/3", with_open_file.0.id()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while fs::read_link(&fd).is_err() {
            assert!(Instant::now() < deadline, "the shell never opened the file");
            std::thread::sleep(Duration::from_millis(20));
        }

        for name in [
            "held-cwd/",
            "held-open/data",
            "held-open",
            "held-cwd",
            "idle",
        ] {
            let path = tmp.join(name);
            if path.exists() {
                age(&path, 48);
            }
        }

        let reap = fixture.gc(&[]);
        let scratch = &reap["scratch"];
        assert_eq!(scratch["entries_removed"], 1, "{reap}");
        assert_eq!(scratch["entries_skipped"], 2, "{reap}");
        assert_eq!(entry(scratch, &tmp, "idle")["action"], "removed");
        for name in ["held-cwd", "held-open"] {
            let skipped = entry(scratch, &tmp, name);
            assert_eq!(skipped["action"], "skipped", "{name}");
            assert!(
                skipped["reason"].as_str().unwrap().contains("live process"),
                "{name}: {skipped}"
            );
            assert!(tmp.join(name).exists(), "{name} was removed while held");
        }

        drop((in_cwd, with_open_file));
        let after = fixture.gc(&[]);
        assert_eq!(after["scratch"]["entries_removed"], 2, "{after}");
        assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
    }
}

#[test]
fn scratch_gc_contracts() {
    if crate::isolated_test::run_in_child("scratch_gc::scratch_gc_contracts") {
        return;
    }
    for (name, check) in [
        (
            "prunes_entries_older_than_the_window_and_never_follows_symlinks",
            prunes_entries_older_than_the_window_and_never_follows_symlinks as fn(),
        ),
        (
            "a_negative_window_is_refused_before_anything_is_removed",
            a_negative_window_is_refused_before_anything_is_removed as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}
