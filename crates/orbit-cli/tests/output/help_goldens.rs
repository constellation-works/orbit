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
    for case in discover_help_golden_cases(&root) {
        let args: Vec<_> = case.argv.iter().map(String::as_str).collect();
        let relative = &case.golden;
        let actual = read_help(&args, &home, &work).replace(env!("CARGO_PKG_VERSION"), "<VERSION>");
        let path = root.join(relative);
        if update {
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
