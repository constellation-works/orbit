//! Family-owned `help_goldens/**/*.cases.json` bind explicit argv to `.txt`
//! goldens (paths relative to `help_goldens/`); discovery needs no Rust registration.
//! The text goldens freeze the shipped `--help` contract of the
//! `orbit` binary. Drift is a consumer-visible CLI surface change. If the new
//! help is intentional, regenerate with `ORBIT_UPDATE_HELP_GOLDENS=1 cargo test
//! -p orbit-cli --test output help_goldens::` (or `make goldens UPDATE=1`) and review
//! the diff.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::tempdir;

const UPDATE_HELP_GOLDENS_ENV: &str = "ORBIT_UPDATE_HELP_GOLDENS";

/// Explicit argv and a path relative to `tests/help_goldens/`, owned by the
/// command family's `*.cases.json` fixture rather than a shared Rust index.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelpGoldenCase {
    argv: Vec<String>,
    golden: String,
}

fn collect_help_fixture_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|err| panic!("list help fixtures {}: {err}", directory.display()))
    {
        let entry = entry.expect("read help fixture entry");
        let path = entry.path();
        let kind = entry.file_type().expect("help fixture file type");
        if kind.is_dir() {
            collect_help_fixture_files(&path, files);
        } else {
            assert!(
                kind.is_file(),
                "help fixture must be a regular file: {}",
                path.display()
            );
            files.push(path);
        }
    }
}

fn discover_help_golden_cases(root: &Path) -> Vec<HelpGoldenCase> {
    let mut files = Vec::new();
    collect_help_fixture_files(root, &mut files);
    files.sort();
    let mut cases = Vec::new();
    let mut argv_sources = BTreeMap::new();
    let mut golden_sources = BTreeMap::new();
    for fixture in &files {
        if !fixture.to_string_lossy().ends_with(".cases.json") {
            continue;
        }
        let contents = fs::read_to_string(fixture)
            .unwrap_or_else(|err| panic!("read help bindings {}: {err}", fixture.display()));
        let bindings: Vec<HelpGoldenCase> = serde_json::from_str(&contents)
            .unwrap_or_else(|err| panic!("invalid help bindings {}: {err}", fixture.display()));
        assert!(
            !bindings.is_empty(),
            "empty help bindings: {}",
            fixture.display()
        );
        for case in bindings {
            assert!(
                case.argv.iter().all(|arg| !arg.is_empty()
                    && !arg.starts_with('-')
                    && !arg.chars().any(char::is_whitespace)),
                "invalid help argv {:?} in {}: use command tokens without flags",
                case.argv,
                fixture.display()
            );
            let relative = Path::new(&case.golden);
            assert!(
                relative.extension().is_some_and(|ext| ext == "txt")
                    && relative
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "invalid help golden path {:?} in {}: expected a relative .txt path without traversal",
                case.golden,
                fixture.display()
            );
            if let Some(previous) = argv_sources.insert(case.argv.clone(), fixture) {
                panic!(
                    "duplicate help argv {:?} in {} and {}",
                    case.argv,
                    previous.display(),
                    fixture.display()
                );
            }
            if let Some(previous) = golden_sources.insert(root.join(relative), fixture) {
                panic!(
                    "duplicate help golden {:?} in {} and {}",
                    case.golden,
                    previous.display(),
                    fixture.display()
                );
            }
            cases.push(case);
        }
    }
    assert!(
        !cases.is_empty(),
        "no help bindings found under {}",
        root.display()
    );
    for file in files
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
    {
        assert!(
            golden_sources.contains_key(file),
            "unreferenced help golden: {}",
            file.display()
        );
    }
    cases
}

fn read_help(args: &[&str], home: &Path, work: &Path) -> String {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_FORMAT")
        .env_remove("ORBIT_SKIP_HOST_PREREQUISITES")
        .args(args)
        .arg("--help")
        .output()
        .expect("run orbit --help");
    assert!(
        output.status.success(),
        "`orbit {} --help` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

fn check_help_goldens(root: &Path, update: bool, home: &Path, work: &Path) {
    for case in discover_help_golden_cases(root) {
        let args: Vec<_> = case.argv.iter().map(String::as_str).collect();
        let relative = &case.golden;
        let actual = read_help(&args, home, work).replace(env!("CARGO_PKG_VERSION"), "<VERSION>");
        let path = root.join(relative);
        if update {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap_or_else(|err| {
                    panic!("create help golden directory {}: {err}", parent.display())
                });
            }
            fs::write(&path, &actual)
                .unwrap_or_else(|err| panic!("write help golden {}: {err}", path.display()));
            continue;
        }
        let expected = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read help golden {}: {err}", path.display()));
        assert_eq!(
            actual,
            expected,
            "`orbit {} --help` drifted from {relative}. If the new help is intentional, \
             regenerate with `{UPDATE_HELP_GOLDENS_ENV}=1 cargo test -p orbit-cli --test \
             output help_goldens::` or `make goldens UPDATE=1`, then review the diff.",
            args.join(" ")
        );
    }
}

#[test]
fn subcommand_globals_are_single_line_references() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");

    for args in [
        &["task", "show"][..],
        &["task", "artifact"],
        &["gc"],
        &["run", "auto"],
        &["workspace", "show"],
        &["audit", "export"],
        &["mcp", "serve"],
    ] {
        let help = read_help(args, &home, &work);
        let (_, globals) = help.split_once("\nGlobal options").unwrap_or_else(|| {
            panic!(
                "{}: inherited options need their own section",
                args.join(" ")
            )
        });
        let entries: Vec<_> = globals
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .collect();
        let mut expected = vec![
            "--root <ROOT>",
            "--workspace <SELECTOR>",
            "--format <MODE>",
            "--json",
            "--header",
        ];
        // These commands own a flag with the same name; its help stays local.
        if args == ["audit", "export"] {
            expected.retain(|entry| !entry.starts_with("--format"));
        }
        if args == ["mcp", "serve"] {
            expected.retain(|entry| !entry.starts_with("--workspace"));
        }
        assert_eq!(
            entries.iter().map(|line| line.trim()).collect::<Vec<_>>(),
            expected,
            "{}: globals must occupy one line each, with descriptions available through orbit --help",
            args.join(" ")
        );
    }
}

#[test]
fn help_matches_the_shipped_surface() {
    // An empty HOME and cwd: no workspace and no installed plugin groups, so
    // the help is the binary's own surface.
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");
    let update = std::env::var(UPDATE_HELP_GOLDENS_ENV).as_deref() == Ok("1");

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/help_goldens");
    check_help_goldens(&root, update, &home, &work);
}

/// A plugin longer than every built-in root group widens their shared
/// description column, and its help wraps against the actual PTY width.
#[cfg(unix)]
#[test]
fn plugin_help_wraps_and_aligns_in_a_pty_and_stays_flat_when_piped() {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let root = temp.path().join("root");
    let empty_root = temp.path().join("empty-root");
    let source = temp.path().join("source");
    fs::create_dir_all(&home).expect("create fixture home");
    fs::create_dir_all(&work).expect("create fixture work");
    fs::create_dir_all(source.join(".orbit-plugin/bin")).expect("create plugin source");
    crate::git_repo::init(&work);

    let fixture_command = |selected_root: &Path| {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("NO_COLOR", "1")
            .env_remove("ORBIT_FORMAT")
            .env_remove("ORBIT_SKIP_HOST_PREREQUISITES")
            .arg("--root")
            .arg(selected_root);
        command
    };

    for (selected_root, machine) in [(&root, "plugin-help"), (&empty_root, "empty-help")] {
        fixture_command(selected_root)
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
                machine,
                "--task-prefix",
                "UXR",
            ])
            .assert()
            .success();
    }
    fixture_command(&root)
        .args(["workspace", "init", "--name", "plugin-help"])
        .assert()
        .success();

    fs::write(
        source.join(".orbit-plugin/plugin.yaml"),
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: long-plugin-namespace\n  version: 0.1.0\n  description: A very long plugin description used to inspect the shared root help column and wrapped output at narrow terminal sizes.\nspec:\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: status\n      description: Report status.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n",
    )
    .expect("write plugin manifest");
    let backend = source.join(".orbit-plugin/bin/backend.sh");
    fs::write(
        &backend,
        "#!/bin/sh\nprintf '{\"ok\":true,\"output\":{}}\\n'\n",
    )
    .expect("write plugin backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o755))
            .expect("make plugin backend executable");
    }
    fixture_command(&root)
        .args(["plugin", "add"])
        .arg(&source)
        .arg("--enable")
        .assert()
        .success();

    let run_pty_help = |width: u16, flag: &str| {
        let mut requested = libc::winsize {
            ws_row: 40,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut master_fd = -1;
        let mut slave_fd = -1;
        // SAFETY: openpty initializes these two valid out-pointers and copies
        // the supplied window size into the newly opened pseudo-terminal.
        // macOS declares the winsize parameter as *mut, Linux as *const. A raw *mut
        // satisfies both (coercing to *const on Linux) and avoids
        // clippy::unnecessary_mut_passed, which fires on `&mut` where *const suffices.
        let opened = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(requested),
            )
        };
        assert_eq!(opened, 0, "open PTY: {}", std::io::Error::last_os_error());
        // SAFETY: openpty succeeded (asserted above), so `master_fd` is an open pty
        // master that no other handle owns; this File takes sole ownership and closes it.
        let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
        // SAFETY: openpty succeeded (asserted above), so `slave_fd` is the distinct, open
        // pty slave of that pair that no other handle owns; this File takes sole ownership.
        let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
        let mut actual = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the slave descriptor is open and `actual` is a valid output
        // pointer for TIOCGWINSZ.
        let read_size = unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ, &mut actual) };
        assert_eq!(read_size, 0, "read PTY window size");
        assert_eq!(actual.ws_col, width, "PTY accepted requested width");

        let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("NO_COLOR", "1")
            .env_remove("ORBIT_FORMAT")
            .env_remove("ORBIT_SKIP_HOST_PREREQUISITES")
            .arg("--root")
            .arg(&root)
            .arg(flag)
            .stdin(Stdio::from(slave.try_clone().expect("clone PTY slave")))
            .stdout(Stdio::from(slave.try_clone().expect("clone PTY stdout")))
            .stderr(Stdio::from(slave.try_clone().expect("clone PTY stderr")));
        let mut child = command.spawn().expect("spawn CLI help in PTY");
        drop(command);
        drop(slave);
        let mut master = master;
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("PTY help timed out at {width} columns ({flag})");
            }
            let timeout_ms = remaining.as_millis().min(5_000) as i32;
            let mut poll_fd = libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            };
            // SAFETY: `poll_fd` points to one initialized pollfd and the call
            // waits at most timeout_ms without mutating unrelated descriptors.
            let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
            if ready == 0 {
                continue;
            }
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = child.kill();
                let _ = child.wait();
                panic!("poll PTY output: {error}");
            }
            let mut chunk = [0_u8; 4096];
            match master.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => output.extend_from_slice(&chunk[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("read PTY output: {error}");
                }
            }
        }
        let status = child.wait().expect("wait for CLI help");
        (
            status,
            String::from_utf8(output)
                .expect("help is UTF-8")
                .replace('\r', ""),
        )
    };

    for width in [60, 80, 120] {
        for flag in ["--help", "-h"] {
            let (status, help) = run_pty_help(width, flag);
            assert!(status.success(), "orbit {flag} failed at {width}: {help}");
            let lines: Vec<&str> = help.lines().collect();
            let heading = lines
                .iter()
                .position(|line| *line == "Plugins:")
                .expect("active plugin section");
            let end = lines[heading + 1..]
                .iter()
                .position(|line| line.is_empty())
                .expect("plugin section boundary");
            let plugin_lines = &lines[heading + 1..heading + 1 + end];
            let plugin_column = plugin_lines[0]
                .find("A very long plugin description")
                .expect("plugin description column");
            let built_in = lines
                .iter()
                .find(|line| line.contains("Initialize the global Orbit root"))
                .expect("built-in root description");
            assert_eq!(
                plugin_column,
                built_in.find("Initialize the global Orbit root").unwrap(),
                "plugin description column at {width} columns"
            );
            assert!(
                plugin_lines
                    .iter()
                    .all(|line| line.chars().count() <= usize::from(width)),
                "plugin row exceeded {width} columns:\n{}",
                plugin_lines.join("\n")
            );
            assert!(
                plugin_lines
                    .iter()
                    .skip(1)
                    .all(|line| line.starts_with(&" ".repeat(plugin_column))),
                "plugin continuation did not hang at its description column:\n{}",
                plugin_lines.join("\n")
            );
        }
    }

    let piped = fixture_command(&root)
        .arg("--help")
        .output()
        .expect("run piped plugin help");
    assert!(piped.status.success());
    let piped_help = String::from_utf8(piped.stdout).expect("piped help is UTF-8");
    let piped_lines: Vec<&str> = piped_help.lines().collect();
    let heading = piped_lines
        .iter()
        .position(|line| *line == "Plugins:")
        .expect("piped plugin section");
    let end = piped_lines[heading + 1..]
        .iter()
        .position(|line| line.is_empty())
        .expect("piped plugin section boundary");
    assert_eq!(end, 1, "piped plugin description stays on one line");

    let no_plugin = fixture_command(&empty_root)
        .arg("--help")
        .output()
        .expect("run help without plugins");
    assert!(no_plugin.status.success());
    assert!(!String::from_utf8_lossy(&no_plugin.stdout).contains("Plugins:"));
}

/// Grouped `orbit run` / `orbit task` help must keep every description in one
/// column with a clear gap, and never stack a description onto its own line in
/// piped output [ORB-19912]. Piped output is width 0, so this is the wide
/// alignment boundary; narrow TTY wrapping is checked separately.
#[test]
fn grouped_command_help_stays_aligned_without_stacked_rows() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");

    let run_sections: &[(&str, &[&str])] = &[
        ("Workflows", &["auto", "ship", "task-pilot", "job"]),
        (
            "Audits",
            &[
                "history",
                "show",
                "logs",
                "events",
                "trace",
                "settlements",
                "settlements-hydrate",
            ],
        ),
        (
            "Maintenance",
            &["cancel", "concurrency", "extend", "readiness", "profiles"],
        ),
    ];
    let task_sections: &[(&str, &[&str])] = &[
        (
            "Tasks",
            &[
                "add", "update", "repilot", "archive", "list", "eligible", "show", "artifact",
            ],
        ),
        (
            "Health",
            &[
                "lint",
                "flow",
                "locks",
                "review-reset",
                "reconcile-review",
                "retry-handoff",
                "recover-claim",
                "revoke-handoff",
                "requeue",
                "recheck-blocked",
                "settle-merged",
            ],
        ),
        ("Bundles", &["export", "import", "publication", "reindex"]),
    ];

    for family in ["run", "task"] {
        let sections = if family == "run" {
            run_sections
        } else {
            task_sections
        };
        for flag in ["--help", "-h"] {
            let help = read_help_with_flag(&[family], flag, &home, &work);
            check_grouped_sections_aligned(family, flag, &help, sections);
        }
    }
}

fn read_help_with_flag(args: &[&str], flag: &str, home: &Path, work: &Path) -> String {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_FORMAT")
        .env_remove("ORBIT_SKIP_HOST_PREREQUISITES")
        .args(args)
        .arg(flag)
        .output()
        .expect("run orbit help");
    assert!(
        output.status.success(),
        "`orbit {} {flag}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

fn check_grouped_sections_aligned(
    family: &str,
    flag: &str,
    help: &str,
    sections: &[(&str, &[&str])],
) {
    let lines: Vec<&str> = help.lines().collect();
    let mut template_columns = Vec::new();
    for (heading, expected) in sections {
        let want = format!("{heading}:");
        let start = lines
            .iter()
            .position(|line| *line == want)
            .unwrap_or_else(|| {
                panic!("`orbit {family} {flag}` missing section `{heading}`");
            });
        let end = lines[start + 1..]
            .iter()
            .position(|line| {
                sections
                    .iter()
                    .any(|(other, _)| *line == format!("{other}:"))
                    || *line == "Options:"
            })
            .map(|offset| start + 1 + offset)
            .unwrap_or(lines.len());
        let mut seen = Vec::new();
        let mut columns = Vec::new();
        for line in &lines[start + 1..end] {
            if line.trim().is_empty() {
                continue;
            }
            assert!(
                line.starts_with("  "),
                "`orbit {family} {flag}` `{heading}` row not indented: {line:?}"
            );
            let first = line.split_whitespace().next().unwrap_or_else(|| {
                panic!("`orbit {family} {flag}` `{heading}` empty row");
            });
            assert!(
                expected.contains(&first),
                "`orbit {family} {flag}` `{heading}` stacked or unknown row: {line:?} \
                 (descriptions must stay on the command line, not a stacked line)"
            );
            seen.push(first);
            let name_pos = line.find(first).expect("row contains its name");
            assert_eq!(
                name_pos, 2,
                "`orbit {family} {flag}` `{heading}` name not at column 2: {line:?}"
            );
            let name_end = name_pos + first.len();
            let gap = line[name_end..]
                .find(|c: char| c != ' ')
                .unwrap_or_else(|| {
                    panic!("`orbit {family} {flag}` `{heading}` missing description: {line:?}")
                });
            assert!(
                gap >= 2,
                "`orbit {family} {flag}` `{heading}` needs a clear separator after `{first}`: {line:?}"
            );
            columns.push(name_end + gap);
        }
        assert_eq!(
            seen.len(),
            expected.len(),
            "`orbit {family} {flag}` `{heading}` missing commands"
        );
        for name in *expected {
            assert!(
                seen.contains(name),
                "`orbit {family} {flag}` `{heading}` missing `{name}`"
            );
        }
        let first_column = columns.first().unwrap_or_else(|| {
            panic!("`orbit {family} {flag}` `{heading}` has no rows");
        });
        for column in &columns {
            assert_eq!(
                column, first_column,
                "`orbit {family} {flag}` `{heading}` descriptions misaligned"
            );
        }
        template_columns.push(*first_column);
    }
    let first = template_columns.first().expect("template has sections");
    for column in &template_columns {
        assert_eq!(
            column, first,
            "`orbit {family} {flag}` sections use different description columns"
        );
    }
}

#[test]
fn update_creates_missing_family_directories_but_validation_stays_strict() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("help_goldens");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&root).expect("fixture root");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");
    let bindings = root.join("new_family.cases.json");
    fs::write(
        &bindings,
        r#"[{"argv":["task","show"],"golden":"new_family/show.txt"}]"#,
    )
    .expect("fixture binding");

    let golden = root.join("new_family/show.txt");
    assert!(!golden.exists(), "fixture golden must start missing");
    check_help_goldens(&root, true, &home, &work);
    assert!(
        golden.is_file(),
        "update mode must create the family directory"
    );
    assert!(
        !fs::read_to_string(&golden)
            .expect("generated help golden")
            .is_empty()
    );

    fs::remove_file(&golden).expect("remove generated golden");
    let missing = std::panic::catch_unwind(|| check_help_goldens(&root, false, &home, &work))
        .expect_err("normal mode must reject a missing golden");
    assert!(
        missing
            .downcast_ref::<String>()
            .is_some_and(|message| message.contains("read help golden")),
        "normal mode must fail while reading the missing golden"
    );

    fs::write(
        &bindings,
        r#"[{"argv":["task","show"],"golden":"../outside.txt"}]"#,
    )
    .expect("invalid fixture binding");
    let invalid = std::panic::catch_unwind(|| check_help_goldens(&root, true, &home, &work))
        .expect_err("update mode must reject an invalid golden path");
    assert!(
        invalid
            .downcast_ref::<String>()
            .is_some_and(|message| message.contains("invalid help golden path")),
        "invalid paths must be rejected before update writes"
    );
    assert!(!temp.path().join("outside.txt").exists());
}

/// `orbit <args> <flag>` on a pseudo-terminal whose width is `COLUMNS`.
#[cfg(unix)]
fn read_help_on_pty(args: &[&str], flag: &str, width: usize) -> String {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::process::Stdio;

    let temp = tempdir().expect("tempdir");
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: openpty initializes both descriptors on success; pointers are valid.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: successful openpty returned owned, distinct file descriptors.
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    // SAFETY: slave is the other owned end of the same openpty pair, wrapped only here.
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let mut child = command
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("COLUMNS", width.to_string())
        .env("NO_COLOR", "1")
        .env_remove("ORBIT_FORMAT")
        .env_remove("ORBIT_SKIP_HOST_PREREQUISITES")
        .args(args)
        .arg(flag)
        .stdout(Stdio::from(slave))
        .spawn()
        .expect("run help on pty");
    drop(command);
    let mut bytes = Vec::new();
    if let Err(error) = master.read_to_end(&mut bytes) {
        // Linux PTY masters signal closure with EIO; other Unix systems return EOF.
        assert_eq!(error.raw_os_error(), Some(libc::EIO), "read pty: {error}");
    }
    assert!(child.wait().expect("wait for help").success());
    String::from_utf8(bytes).expect("UTF-8 help")
}

#[cfg(unix)]
#[test]
fn grouped_help_fits_narrow_tty() {
    for args in [&[][..], &["run"][..], &["task"][..]] {
        for width in [8usize, 16, 20] {
            let output = read_help_on_pty(args, "--help", width);
            let start_heading = match args.first().copied() {
                Some("run") => "Workflows:",
                Some("task") => "Tasks:",
                _ => "Setup:",
            };
            let grouped = output
                .split_once(start_heading)
                .expect("grouped heading")
                .1
                .split_once("Options:")
                .expect("options heading")
                .0;
            for line in grouped.lines().map(|line| line.trim_end_matches('\r')) {
                if line.chars().count() > width {
                    let words: Vec<_> = line.split_whitespace().collect();
                    let indent = line.chars().take_while(|c| *c == ' ').count();
                    assert!(
                        words.len() == 1 && words[0].chars().count() > width.saturating_sub(indent),
                        "orbit {args:?}, width {width}: overlong grouped line {line:?}"
                    );
                }
            }
        }
    }
}

/// Clap's own help text (flags, about, command lists, footers) used to come
/// out as one long line per paragraph because Clap's `wrap_help` was off, so a
/// terminal soft-wrapped it from column 0. At a terminal width every line must
/// now fit, at 120 as well as at 60, and a continuation must stay indented
/// under the flag or command it belongs to.
#[cfg(unix)]
#[test]
fn clap_help_wraps_to_the_terminal_width_with_a_hanging_indent() {
    let surfaces: [(&[&str], &str); 7] = [
        (&[], "-h"),
        (&["search"], "--help"),
        (&["config", "browser-kit"], "--help"),
        (&["workspace"], "--help"),
        (&["run"], "--help"),
        (&["task"], "--help"),
        (&["task", "update"], "--help"),
    ];
    for width in [60usize, 80, 120] {
        for (args, flag) in surfaces {
            let output = read_help_on_pty(args, flag, width);
            let mut previous_indent = 0usize;
            for line in output.lines().map(|line| line.trim_end_matches('\r')) {
                let indent = line.chars().take_while(|c| *c == ' ').count();
                let words: Vec<_> = line.split_whitespace().collect();
                assert!(
                    line.chars().count() <= width
                        || (words.len() == 1
                            && words[0].chars().count() > width.saturating_sub(indent)),
                    "orbit {args:?} {flag}, width {width}: overlong line {line:?}"
                );
                // A wrapped flag description sits deeper than its flag, never at column 0.
                if previous_indent >= 6 && !line.is_empty() {
                    assert!(
                        indent > 0,
                        "orbit {args:?} {flag}, width {width}: continuation at column 0 {line:?}"
                    );
                }
                previous_indent = indent;
            }
        }
    }
}

#[test]
fn public_commands_appear_in_grouped_help() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");
    for flag in ["-h", "--help"] {
        for (args, group, names) in [
            (&[][..], "Advanced", &["application"][..]),
            (&[][..], "Observe", &["attention"][..]),
            (&["run"][..], "Audits", &["settlements-hydrate"][..]),
        ] {
            let mut command = cargo_bin_cmd!("orbit");
            test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            let output = command
                .current_dir(&work)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .args(args)
                .arg(flag)
                .output()
                .expect("run help");
            assert!(output.status.success());
            let help = String::from_utf8(output.stdout).expect("UTF-8 help");
            let section = help
                .split_once(&format!("{group}:\n"))
                .expect("group")
                .1
                .split("\n\n")
                .next()
                .expect("rows");
            for name in names {
                assert!(
                    section
                        .lines()
                        .any(|line| line.split_whitespace().next() == Some(name)),
                    "{name} missing from {group} in {flag}: {section}"
                );
            }
        }
    }
}
