use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use orbit_common::OrbitError;

use super::super::inspect::RunningBinary;
use super::super::manager::{ClockCommandRunner, ManagerCommand, ManagerCommandOutput};
use super::super::status::LaunchdHealthProbe;

pub(super) struct MockRunner {
    results: Mutex<Vec<Result<bool, OrbitError>>>,
    outputs: Mutex<Vec<Result<Option<String>, OrbitError>>>,
    probes: Mutex<Vec<Result<ManagerCommandOutput, OrbitError>>>,
    commands: Mutex<Vec<String>>,
}

impl MockRunner {
    pub(super) fn new(results: Vec<Result<bool, OrbitError>>) -> Self {
        Self {
            results: Mutex::new(results.into_iter().rev().collect()),
            outputs: Mutex::new(Vec::new()),
            probes: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn with_outputs(
        results: Vec<Result<bool, OrbitError>>,
        outputs: Vec<Result<Option<String>, OrbitError>>,
    ) -> Self {
        Self {
            results: Mutex::new(results.into_iter().rev().collect()),
            outputs: Mutex::new(outputs.into_iter().rev().collect()),
            probes: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn with_probes(
        results: Vec<Result<bool, OrbitError>>,
        outputs: Vec<Result<Option<String>, OrbitError>>,
        probes: Vec<Result<ManagerCommandOutput, OrbitError>>,
    ) -> Self {
        Self {
            results: Mutex::new(results.into_iter().rev().collect()),
            outputs: Mutex::new(outputs.into_iter().rev().collect()),
            probes: Mutex::new(probes.into_iter().rev().collect()),
            commands: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("test command log lock").clone()
    }
}

impl ClockCommandRunner for MockRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        self.results
            .lock()
            .expect("test result queue lock")
            .pop()
            .expect("test configured a result for every manager command")
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        self.outputs
            .lock()
            .expect("test output queue lock")
            .pop()
            .expect("test configured output for every manager query")
    }

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        if let Some(output) = self.probes.lock().expect("test probe queue lock").pop() {
            return output;
        }
        let success = self
            .results
            .lock()
            .expect("test result queue lock")
            .pop()
            .expect("test configured a result for every manager command")?;
        Ok(ManagerCommandOutput {
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// Program the test launchd unit names. It is deliberately absent from disk so
/// the real `probe_program_version` reports the observed missing-program case.
pub(super) const INSTALLED_PROGRAM: &str = "/opt/homebrew/bin/orbit";
/// launchd domain owner the fake transcripts belong to.
const TEST_UID: u32 = 501;

/// Install a launchd plist naming `program`, as `orbit routine init` would.
pub(super) fn write_launchd_unit(home: &Path, program: &str) -> PathBuf {
    let agents = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents).expect("launchd agents dir");
    let path = agents.join("com.orbit.sweep.plist");
    fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.orbit.sweep</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
        <string>clock</string>
        <string>tick</string>
    </array>
</dict>
</plist>
"#
        ),
    )
    .expect("write launchd plist");
    path
}

/// A unit program that answers `--version` with this binary's version, so the
/// unit inspection is satisfied and only the transcript decides health.
pub(super) fn installed_version(_: &Path) -> Result<String, String> {
    Ok(env!("CARGO_PKG_VERSION").to_string())
}

pub(super) fn launchd_probe(
    home: &Path,
    version_probe: fn(&Path) -> Result<String, String>,
) -> LaunchdHealthProbe {
    LaunchdHealthProbe {
        home: home.to_path_buf(),
        uid: TEST_UID,
        running: RunningBinary {
            path: PathBuf::from("/Users/tester/.orbit/bin/orbit"),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        version_probe,
    }
}

/// An abridged `launchctl print gui/<uid>/com.orbit.sweep` dump. The nested
/// blocks are kept because they repeat key names the parser must not read.
pub(super) fn launchctl_print(last_exit_line: &str, properties: &str) -> String {
    format!(
        "gui/{TEST_UID}/com.orbit.sweep = {{
\tactive count = 0
\tpath = /Users/tester/Library/LaunchAgents/com.orbit.sweep.plist
\tstate = spawn scheduled

\tprogram = {INSTALLED_PROGRAM}
\tdomain = gui/{TEST_UID} [100002]
\truns = 1
\t{last_exit_line}

\tevent channels = {{
\t\t\"com.apple.launchd.helper\" = {{
\t\t\tstate = active
\t\t}}
\t}}

\tjetsamproperties category = daemon
\tproperties = {properties}
}}
"
    )
}

pub(super) fn manager_output(success: bool, stdout: &str, stderr: &str) -> ManagerCommandOutput {
    ManagerCommandOutput {
        success,
        exit_code: Some(if success { 0 } else { 1 }),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

#[derive(Debug)]
struct FakeSystemdState {
    now_seconds: u64,
    enabled: bool,
    active: bool,
    last_service_activation: Option<u64>,
    next_trigger: Option<u64>,
}

/// A temporal systemd fake: startup-relative deadlines are based on the
/// already-running manager, timer-relative deadlines are based on every
/// restart, and service-relative deadlines are recomputed after a sweep.
pub(super) struct SystemdManagerFake {
    home: PathBuf,
    state: Mutex<FakeSystemdState>,
    commands: Mutex<Vec<String>>,
}

impl SystemdManagerFake {
    pub(super) fn new(home: &Path, now_seconds: u64) -> Self {
        Self {
            home: home.to_path_buf(),
            state: Mutex::new(FakeSystemdState {
                now_seconds,
                enabled: false,
                active: false,
                last_service_activation: None,
                next_trigger: None,
            }),
            commands: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn late_elapsed(
        home: &Path,
        now_seconds: u64,
        last_service_activation: u64,
    ) -> Self {
        Self {
            home: home.to_path_buf(),
            state: Mutex::new(FakeSystemdState {
                now_seconds,
                enabled: true,
                active: true,
                last_service_activation: Some(last_service_activation),
                next_trigger: None,
            }),
            commands: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn next_trigger(&self) -> Option<u64> {
        self.state
            .lock()
            .expect("fake systemd state lock")
            .next_trigger
    }

    pub(super) fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("fake command log lock").clone()
    }

    pub(super) fn set_now(&self, now_seconds: u64) {
        self.state
            .lock()
            .expect("fake systemd state lock")
            .now_seconds = now_seconds;
    }

    pub(super) fn elapse_and_complete_service(&self) {
        let cadence = self
            .timer_value("OnUnitActiveSec")
            .expect("recurring timer directive");
        let mut state = self.state.lock().expect("fake systemd state lock");
        let triggered_at = state.next_trigger.expect("timer has a next trigger");
        state.now_seconds = triggered_at;
        state.last_service_activation = Some(triggered_at);
        state.next_trigger = Some(triggered_at + cadence);
    }

    fn timer_value(&self, directive: &str) -> Option<u64> {
        let timer = fs::read_to_string(self.home.join(".config/systemd/user/orbit-sweep.timer"))
            .expect("fake manager reads installed timer");
        timer.lines().find_map(|line| {
            line.strip_prefix(&format!("{directive}="))
                .and_then(|value| value.strip_suffix('s'))
                .and_then(|value| value.parse().ok())
        })
    }

    fn activate_timer(&self) {
        let on_active = self.timer_value("OnActiveSec");
        let on_startup = self.timer_value("OnStartupSec");
        let on_unit_active = self.timer_value("OnUnitActiveSec");
        let mut state = self.state.lock().expect("fake systemd state lock");
        let now = state.now_seconds;
        let activation_deadline = on_active.map(|cadence| now + cadence);
        // This models the late `active (elapsed)` regression: startup and
        // previous-service deadlines that elapsed before restart do not
        // establish a new future trigger.
        let startup_deadline = on_startup.filter(|deadline| *deadline > now);
        let recurring_deadline = on_unit_active
            .zip(state.last_service_activation)
            .map(|(cadence, activated)| activated + cadence)
            .filter(|deadline| *deadline > now);
        state.active = true;
        state.next_trigger = [activation_deadline, startup_deadline, recurring_deadline]
            .into_iter()
            .flatten()
            .min();
    }
}

impl ClockCommandRunner for SystemdManagerFake {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        let display = command.display();
        self.commands
            .lock()
            .expect("fake command log lock")
            .push(display.clone());
        match display.as_str() {
            "systemctl --user daemon-reload" => Ok(true),
            "systemctl --user is-enabled orbit-sweep.timer" => {
                Ok(self.state.lock().expect("fake systemd state lock").enabled)
            }
            "systemctl --user enable orbit-sweep.timer" => {
                self.state.lock().expect("fake systemd state lock").enabled = true;
                Ok(true)
            }
            "systemctl --user restart orbit-sweep.timer" => {
                self.activate_timer();
                Ok(true)
            }
            "systemctl --user disable --now orbit-sweep.timer" => {
                let mut state = self.state.lock().expect("fake systemd state lock");
                state.enabled = false;
                state.active = false;
                state.next_trigger = None;
                Ok(true)
            }
            unexpected => panic!("unexpected fake systemd command: {unexpected}"),
        }
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        self.commands
            .lock()
            .expect("fake command log lock")
            .push(command.display());
        let state = self.state.lock().expect("fake systemd state lock");
        let next = state
            .next_trigger
            .map_or_else(|| "infinity".to_string(), |value| format!("{value}s"));
        let last = state
            .last_service_activation
            .map_or_else(String::new, |value| format!("{value}s"));
        Ok(Some(format!(
            "LoadState=loaded\nActiveState={}\nNextElapseUSecRealtime=\nNextElapseUSecMonotonic={next}\nLastTriggerUSec={last}",
            if state.active { "active" } else { "inactive" }
        )))
    }
}

pub(super) fn service_path(rendered: &str) -> &str {
    rendered
        .lines()
        .find_map(|line| line.strip_prefix("Environment=PATH="))
        .expect("systemd service declares PATH")
}

pub(super) fn finds_launcher(path: &str, launcher: &str) -> bool {
    path.split(':')
        .map(Path::new)
        .any(|directory| directory.join(launcher).is_file())
}

pub(super) fn write_stale_on_startup_timer(home: &Path, cadence_seconds: u64) {
    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("create systemd user unit dir");
    fs::write(
        unit_dir.join("orbit-sweep.timer"),
        format!(
            "# stale pre-OnActiveSec timer\n[Unit]\nDescription=Run orbit sweep every {cadence_seconds} seconds\n\n[Timer]\nOnStartupSec={cadence_seconds}s\nOnUnitActiveSec={cadence_seconds}s\nAccuracySec=5s\n\n[Install]\nWantedBy=timers.target\n"
        ),
    )
    .expect("write stale OnStartupSec timer");
}

pub(super) fn systemd_show_command() -> &'static str {
    "systemctl --user show orbit-sweep.timer --property=LoadState --property=ActiveState --property=NextElapseUSecRealtime --property=NextElapseUSecMonotonic --property=LastTriggerUSec"
}
