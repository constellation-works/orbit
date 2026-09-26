//! `orbit plugin secret set|list|rm` end to end (design `docs/design/plugins`
//! §3, "Plugin secrets").
//!
//! The claim under test is where a value may appear: in the host-owned secret
//! store and nowhere else. Every step runs the real binary against a
//! disposable home, then the whole Orbit root outside the store — logs, the
//! audit database, plugin records — is searched for the value's bytes, beside
//! every command's stdout and stderr.
#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

/// Distinctive enough that finding it anywhere is a leak, not a coincidence.
const SECRET: &str = "orbit-test-secret-7f3a9c1e5b";

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    /// Everything any command printed, searched for the value at the end.
    printed: std::cell::RefCell<String>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");
        let fixture = Self {
            _temp: temp,
            home,
            work,
            printed: std::cell::RefCell::default(),
        };
        fixture.run_ok(&[
            "init",
            "--non-interactive",
            "--machine-name",
            "plugin-secret-host",
            "--task-prefix",
            "TST",
        ]);
        fixture.run_ok(&["workspace", "init", "--name", "plugin-secrets"]);
        fixture
    }

    fn orbit(&self) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        for name in ["ORBIT_TASK_ACTOR_KIND", "ORBIT_ACTIVITY_TOOLS"] {
            command.env_remove(name);
        }
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home);
        command
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> std::process::Output {
        let mut command = self.orbit();
        command.args(args);
        command.write_stdin(stdin.unwrap_or_default());
        let output = command.output().expect("run orbit");
        let mut printed = self.printed.borrow_mut();
        printed.push_str(&String::from_utf8_lossy(&output.stdout));
        printed.push_str(&String::from_utf8_lossy(&output.stderr));
        output
    }

    fn run_ok(&self, args: &[&str]) -> std::process::Output {
        self.run_ok_with_stdin(args, None)
    }

    fn run_ok_with_stdin(&self, args: &[&str], stdin: Option<&str>) -> std::process::Output {
        let output = self.run(args, stdin);
        assert!(
            output.status.success(),
            "`orbit {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut full = args.to_vec();
        full.extend(["--format", "json"]);
        let output = self.run_ok(&full);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`orbit {}` did not print JSON ({error}): {}",
                full.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn orbit_root(&self) -> PathBuf {
        self.home.join(".orbit")
    }

    fn secret_store(&self) -> PathBuf {
        self.orbit_root().join("state/plugin-secrets")
    }

    fn source(&self) -> PathBuf {
        self.home.join("plugin-sources/vault")
    }
}

/// A plugin declaring one rotatable secret and one plain one.
fn write_plugin(root: &Path, secrets: &str) {
    write_plugin_named(root, "vault", secrets);
}

/// The backend every fixture plugin runs: it reports what its request carried
/// and where else [`SECRET`] was visible to it, without ever echoing it — the
/// tool's output is a surface the value must not reach.
///
/// - `delivered`: stdin held `refresh_token` as `{value: SECRET, version}`;
/// - `version`: that entry's version;
/// - `api_key`: stdin named the unset `api_key` at all;
/// - `env_hits` / `argv_hits`: lines of the environment or argv holding it.
///
/// The script spells the value in two quoted halves, so the installed script
/// is not itself a file the leak scan finds it in.
fn probe_backend() -> String {
    let (head, tail) = SECRET.split_at(SECRET.len() / 2);
    format!(
        "#!/bin/sh\n\
         input=$(cat)\n\
         needle='{head}''{tail}'\n\
         delivered=false\n\
         case \"$input\" in *\"\\\"refresh_token\\\":{{\\\"value\\\":\\\"$needle\\\",\\\"version\\\":\\\"\"*) delivered=true;; esac\n\
         api_key=false\n\
         case \"$input\" in *'\"api_key\"'*) api_key=true;; esac\n\
         version=$(printf '%s' \"$input\" | sed -n 's/.*\"refresh_token\":{{\"value\":\"[^\"]*\",\"version\":\"\\([0-9a-f]*\\)\".*/\\1/p')\n\
         env_hits=$(env | grep -c -F \"$needle\" || true)\n\
         argv_hits=$(printf '%s\\n' \"$0\" \"$@\" | grep -c -F \"$needle\" || true)\n\
         printf '{{\"ok\":true,\"output\":{{\"delivered\":%s,\"version\":\"%s\",\"api_key\":%s,\"env_hits\":%s,\"argv_hits\":%s}}}}\\n' \
         \"$delivered\" \"$version\" \"$api_key\" \"$env_hits\" \"$argv_hits\"\n"
    )
}

fn write_plugin_named(root: &Path, name: &str, secrets: &str) {
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(&backend, probe_backend()).expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 0.1.0\n  \
             description: Secret fixture.\nspec:\n  backend:\n    type: exec\n    command: \
             bin/backend.sh\n  tools:\n    - name: status\n      description: Report.\n      \
             execution_kind: read_only\n      mcp_scope: workspace\n  secrets:\n{secrets}"
        ),
    )
    .expect("write manifest");
}

const TWO_SECRETS: &str = "    - name: refresh_token\n      description: OAuth refresh token.\n      rotatable: true\n    - name: api_key\n";

/// Every file under `dir` whose bytes contain [`SECRET`], skipping `skip`.
fn files_holding_the_secret(dir: &Path, skip: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.starts_with(skip) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            found.extend(files_holding_the_secret(&path, skip));
        } else if file_type.is_file()
            && std::fs::read(&path).is_ok_and(|bytes| {
                bytes
                    .windows(SECRET.len())
                    .any(|window| window == SECRET.as_bytes())
            })
        {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_secret_is_set_from_stdin_and_appears_on_no_other_surface() {
    let fixture = Fixture::new();
    write_plugin(&fixture.source(), TWO_SECRETS);

    // Enabling names every declared secret that has no value yet.
    let added = fixture.json(&[
        "plugin",
        "add",
        fixture.source().to_str().expect("utf8 source"),
        "--enable",
    ]);
    let warnings = added["warnings"].to_string();
    assert!(
        warnings.contains("secret `refresh_token` is declared but not set")
            && warnings.contains("secret `api_key` is declared but not set"),
        "enable must name the unset secrets: {added}"
    );

    // A value on the command line is refused, and the refusal does not
    // repeat it.
    let refused = fixture.run(
        &["plugin", "secret", "set", "vault", "refresh_token", SECRET],
        None,
    );
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("never taken from the command line"),
        "argv values must be refused: {stderr}"
    );
    // A name the manifest does not declare is refused.
    let undeclared = fixture.run(&["plugin", "secret", "set", "vault", "other"], Some(SECRET));
    assert!(!undeclared.status.success());
    assert!(
        String::from_utf8_lossy(&undeclared.stderr).contains("does not declare a secret"),
        "{}",
        String::from_utf8_lossy(&undeclared.stderr)
    );

    fixture.run_ok_with_stdin(
        &["plugin", "secret", "set", "vault", "refresh_token"],
        Some(&format!("{SECRET}\n")),
    );

    let listed = fixture.json(&["plugin", "secret", "list", "vault"]);
    let rows = listed.as_array().expect("secret rows");
    assert_eq!(rows.len(), 2, "{listed}");
    assert_eq!(rows[0]["name"], "refresh_token");
    assert_eq!(rows[0]["set"], true);
    assert_eq!(rows[0]["rotatable"], true);
    assert!(rows[0]["updated_at"].is_string());
    assert_eq!(rows[1]["name"], "api_key");
    assert_eq!(rows[1]["set"], false);
    fixture.run_ok(&["plugin", "secret", "list", "vault"]);

    // `doctor` still names the one that is unset, and not the one that is.
    let doctor = fixture.run(&["plugin", "doctor", "--format", "json"], None);
    let doctor: Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
    let messages: Vec<&str> = doctor
        .as_array()
        .expect("doctor rows")
        .iter()
        .filter_map(|row| row["message"].as_str())
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("declares secret `api_key` but it is not set")),
        "{doctor}"
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.contains("`refresh_token` but it is not set")),
        "{doctor}"
    );

    fixture.json(&["plugin", "show", "vault"]);
    fixture.json(&["plugin", "list"]);

    // The value is stored — once, and privately.
    let stored = std::fs::read_to_string(fixture.secret_store().join("vault.json"))
        .expect("the secret store holds the plugin's file");
    assert!(stored.contains(SECRET), "the value is what was piped in");
    assert!(
        !stored.contains(&format!("{SECRET}\\n")),
        "the trailing newline from stdin is not part of the value"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(fixture.secret_store().join("vault.json"))
            .expect("secret file metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The scan below would find it: pointed at the store, it does.
    assert_eq!(
        files_holding_the_secret(&fixture.secret_store(), Path::new("/nonexistent")),
        vec![fixture.secret_store().join("vault.json")]
    );
    // …and nowhere else: not in anything printed, not in any log, audit row
    // or record under the Orbit root.
    assert!(
        !fixture.printed.borrow().contains(SECRET),
        "a command printed the secret value"
    );
    let leaks = files_holding_the_secret(&fixture.orbit_root(), &fixture.secret_store());
    assert!(
        leaks.is_empty(),
        "the secret value reached files outside the store: {leaks:?}"
    );

    // `rm` removes one secret.
    let removed = fixture.json(&["plugin", "secret", "rm", "vault", "refresh_token"]);
    assert_eq!(removed["removed"], true);
    let listed = fixture.json(&["plugin", "secret", "list", "vault"]);
    assert_eq!(listed[0]["set"], false, "{listed}");
}

#[test]
fn remove_deletes_the_plugins_secrets_unless_record_only() {
    let fixture = Fixture::new();
    write_plugin(&fixture.source(), TWO_SECRETS);
    let source = fixture.source();
    let source = source.to_str().expect("utf8 source");
    fixture.run_ok(&["plugin", "add", source]);
    fixture.run_ok_with_stdin(
        &["plugin", "secret", "set", "vault", "api_key"],
        Some(SECRET),
    );

    fixture.run_ok(&["plugin", "remove", "vault", "--yes", "--record-only"]);
    assert!(
        fixture.secret_store().join("vault.json").exists(),
        "--record-only removes the record and nothing else"
    );

    fixture.run_ok(&["plugin", "add", source, "--force"]);
    fixture.run_ok(&["plugin", "remove", "vault", "--yes"]);
    assert!(
        !fixture.secret_store().join("vault.json").exists(),
        "an ordinary remove deletes the plugin's secrets"
    );
}

/// `orbit tool run` against a plugin whose declared secret the operator set:
/// the backend finds it — value and version — under `context.secrets` on
/// stdin, and nowhere in its environment or argv. The unset declared secret is
/// omitted, and a second plugin declaring the same name receives nothing. The
/// call's audit row names the delivered secret, and no printed output, log,
/// audit row or record outside the store holds the value.
#[cfg(unix)]
#[test]
fn a_set_secret_reaches_its_exec_backend_on_stdin_only() {
    let fixture = Fixture::new();
    write_plugin(&fixture.source(), TWO_SECRETS);
    let other = fixture.home.join("plugin-sources/other");
    write_plugin_named(&other, "other", TWO_SECRETS);
    for source in [fixture.source(), other] {
        fixture.run_ok(&["plugin", "add", source.to_str().expect("utf8"), "--enable"]);
    }
    fixture.run_ok_with_stdin(
        &["plugin", "secret", "set", "vault", "refresh_token"],
        Some(SECRET),
    );

    let output = fixture.run_ok(&["tool", "run", "vault.status", "--input", "{}"]);
    let report: Value = serde_json::from_slice(&output.stdout).expect("tool output JSON");
    assert_eq!(report["delivered"], true, "{report}");
    assert_eq!(
        report["api_key"], false,
        "an unset secret is omitted: {report}"
    );
    assert_eq!(report["env_hits"], 0, "not in the environment: {report}");
    assert_eq!(report["argv_hits"], 0, "not in argv: {report}");
    let stored: Value = serde_json::from_slice(
        &std::fs::read(fixture.secret_store().join("vault.json")).expect("read the store"),
    )
    .expect("store JSON");
    assert_eq!(
        report["version"], stored["secrets"]["refresh_token"]["version"],
        "the delivered version is the stored one"
    );

    let other = fixture.run_ok(&["tool", "run", "other.status", "--input", "{}"]);
    let other: Value = serde_json::from_slice(&other.stdout).expect("tool output JSON");
    assert_eq!(
        other["delivered"], false,
        "another plugin's secret of the same name is not delivered: {other}"
    );

    let conn = rusqlite::Connection::open(fixture.orbit_root().join("orbit.db"))
        .expect("open the audit database");
    let delivered = |tool: &str| -> Vec<Option<String>> {
        conn.prepare("SELECT plugin_secrets FROM audit_events WHERE tool_name = ?1")
            .expect("prepare")
            .query_map([tool], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    };
    assert_eq!(
        delivered("vault.status"),
        vec![Some("[\"refresh_token\"]".to_string())]
    );
    assert_eq!(delivered("other.status"), vec![None]);

    fixture.json(&["plugin", "show", "vault"]);
    fixture.json(&["audit", "list"]);
    assert!(
        !fixture.printed.borrow().contains(SECRET),
        "a command printed the secret value"
    );
    let leaks = files_holding_the_secret(&fixture.orbit_root(), &fixture.secret_store());
    assert!(
        leaks.is_empty(),
        "the secret value reached files outside the store: {leaks:?}"
    );
}
