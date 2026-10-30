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
        ("Workflows", &["auto", "ship", "task-pilot", "job", "agent"]),
        (
            "Audits",
            &["history", "show", "logs", "events", "trace", "settlements"],
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
                "add", "update", "archive", "list", "eligible", "show", "artifact",
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

#[cfg(unix)]
#[test]
fn grouped_help_fits_narrow_tty() {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::process::Stdio;

    let temp = tempdir().expect("tempdir");
    for args in [&[][..], &["run"][..], &["task"][..]] {
        for width in [8usize, 16, 20] {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: openpty initializes both descriptors on success; pointers are valid.
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
            // SAFETY: successful openpty returned owned, distinct file descriptors.
            let mut master = unsafe { fs::File::from_raw_fd(master) };
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
                .arg("--help")
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
            let output = String::from_utf8(bytes).expect("UTF-8 help");
            let start_heading = match args.first().copied() {
                Some("run") => "Workflows:",
                Some("task") => "Tasks:",
                _ => "Environment:",
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
