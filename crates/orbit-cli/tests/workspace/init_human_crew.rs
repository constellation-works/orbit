//! `orbit init` seeds the operator's human crew (`[crews.<name>] kind =
//! "human"`): the OS user without a terminal, an answer at one, or
//! `--human-name`; a re-run leaves it alone, and a name an agent crew holds is
//! re-asked at a terminal and refused without one, before anything is written.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use orbit_common::test_env;
use tempfile::{TempDir, tempdir};

struct HumanCrewFixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl HumanCrewFixture {
    fn new() -> Self {
        let temp = tempdir().expect("fixture tempdir");
        let fixture = Self {
            home: temp.path().join("home"),
            work: temp.path().join("work"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).expect("fixture home");
        fs::create_dir_all(&fixture.work).expect("fixture work");
        crate::git_repo::seal_lookup_boundary(&fixture.work);
        fixture
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".orbit").join("config.toml")
    }

    fn command(&self, user: &str) -> std::process::Command {
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
            .env("PATH", self.work.join("empty-path"))
            .env("USER", user)
            .env_remove("USERNAME")
            .env_remove("LOGNAME")
            .env_remove("RUST_LOG")
            .args(["init", "--machine-name", "qa-host", "--task-prefix", "QA"]);
        command
    }

    fn init(&self, user: &str, flags: &[&str]) -> Output {
        self.command(user)
            .args(["--format", "json"])
            .args(flags)
            .output()
            .expect("run orbit init")
    }

    /// The human crews the global config declares, by name.
    fn human_crews(&self) -> Vec<String> {
        let text = fs::read_to_string(self.config_path()).expect("global config");
        let document = text.parse::<toml::Table>().expect("config parses");
        document
            .get("crews")
            .and_then(toml::Value::as_table)
            .map(|crews| {
                crews
                    .iter()
                    .filter(|(_, entry)| {
                        entry.get("kind").and_then(toml::Value::as_str) == Some("human")
                    })
                    .map(|(name, _)| name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn human_crew_report(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "orbit init: {output:?}");
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).expect("init JSON");
    doc["human_crew"].clone()
}

#[test]
fn non_interactive_init_seeds_the_os_user_and_a_rerun_keeps_it() {
    let fixture = HumanCrewFixture::new();
    let first = human_crew_report(&fixture.init("Daniel.K", &["--non-interactive"]));
    assert_eq!(first["outcome"], "created");
    assert_eq!(first["name"], "daniel-k");
    assert_eq!(fixture.human_crews(), ["daniel-k"]);

    // A re-run, even naming another crew, leaves the existing one alone.
    let before = fs::read_to_string(fixture.config_path()).expect("config");
    let rerun = human_crew_report(&fixture.init(
        "someone-else",
        &["--non-interactive", "--human-name", "other"],
    ));
    assert_eq!(rerun["outcome"], "unchanged");
    assert_eq!(rerun["name"], "daniel-k");
    assert_eq!(fixture.human_crews(), ["daniel-k"]);
    let after = fs::read_to_string(fixture.config_path()).expect("config");
    let crews_of = |text: &str| -> toml::Value {
        text.parse::<toml::Table>().expect("config")["crews"]["daniel-k"].clone()
    };
    assert_eq!(crews_of(&before), crews_of(&after));
}

#[test]
fn piped_init_without_a_terminal_takes_the_os_user_and_the_flag_overrides_it() {
    // No `--non-interactive`, but stdin is no terminal: no prompt is shown.
    let piped = HumanCrewFixture::new();
    let output = piped
        .command("ops")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run orbit init");
    assert!(output.status.success(), "orbit init: {output:?}");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Human crew name"),
        "a non-terminal stdin must not be prompted: {output:?}"
    );
    assert_eq!(piped.human_crews(), ["ops"]);

    let flagged = HumanCrewFixture::new();
    let report =
        human_crew_report(&flagged.init("ops", &["--non-interactive", "--human-name", "Pat Lee"]));
    assert_eq!(report["outcome"], "created");
    assert_eq!(report["name"], "pat-lee");
    assert_eq!(flagged.human_crews(), ["pat-lee"]);
}

#[test]
fn non_interactive_collision_with_an_agent_crew_is_refused_before_writing() {
    for (user, flags, names) in [
        ("opus", &["--non-interactive"][..], "the OS user 'opus'"),
        (
            "dk",
            &["--non-interactive", "--human-name", "sonnet"][..],
            "--human-name 'sonnet'",
        ),
    ] {
        let fixture = HumanCrewFixture::new();
        let output = fixture.init(user, flags);
        assert!(
            !output.status.success(),
            "{user}: collision must fail: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(names), "{user}: {stderr}");
        assert!(stderr.contains("agent crew"), "{user}: {stderr}");
        assert!(
            !fixture.config_path().exists(),
            "{user}: a refused human crew must leave no partial root"
        );
    }
}

#[cfg(unix)]
#[test]
fn interactive_init_offers_the_os_user_and_re_asks_on_an_agent_crew_name() {
    use std::io::Write;
    use std::os::fd::FromRawFd;

    let fixture = HumanCrewFixture::new();
    let (mut controller, terminal) = open_pty();
    let mut command = fixture.command("dk");
    command
        .stdin(std::process::Stdio::from(terminal))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = command.spawn().expect("spawn orbit init");
    // Release the parent's copy of the terminal side with the command.
    drop(command);
    // `haiku` is an agent crew, so the prompt asks again; an empty answer
    // takes the offered OS user.
    controller.write_all(b"haiku\n\n").expect("answer prompts");
    let output = child.wait_with_output().expect("orbit init exits");
    drop(controller);
    assert!(output.status.success(), "orbit init: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[dk]"),
        "the OS user is the offered default: {stderr}"
    );
    assert!(
        stderr.contains("'haiku' is already an agent crew"),
        "{stderr}"
    );
    assert_eq!(fixture.human_crews(), ["dk"]);

    fn open_pty() -> (fs::File, fs::File) {
        let mut controller = -1;
        let mut terminal = -1;
        // SAFETY: openpty writes two fresh descriptors into the out-params on
        // success; each is wrapped once, so each is closed exactly once.
        let rc = unsafe {
            libc::openpty(
                &mut controller,
                &mut terminal,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
        // SAFETY: openpty returned 0 (asserted above), so both descriptors are fresh and
        // owned here; each is wrapped exactly once.
        unsafe {
            (
                fs::File::from_raw_fd(controller),
                fs::File::from_raw_fd(terminal),
            )
        }
    }
}
