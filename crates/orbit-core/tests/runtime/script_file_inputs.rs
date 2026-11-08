//! Declared script file inputs (`files.inputs`) at the runtime's script-step
//! boundary: each input is admitted before spawn from the cwd tree or a
//! granted read root, recorded with its source and SHA-256, and bound
//! read-only at `in/<as>`; a missing or out-of-bounds input refuses the step
//! naming it, and nothing runs.

use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use orbit_engine::{RuntimeHost, ScriptStepEnd, ScriptStepRequest, ScriptStepRun};
use orbit_types::workflow::activity_job::{
    ScriptExecutionRecord, script_refusal, script_sha256_hex,
};
use serde_json::{Value, json};

struct Fixture {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    library: PathBuf,
    /// Written by the entry when it runs; must stay absent after a refusal.
    ran_marker: PathBuf,
}

/// A workspace with a granted `check` script whose entry probes its inputs,
/// and a granted read root `library/` outside the workspace.
fn fixture(inputs_yaml: &str) -> Fixture {
    crate::scenarios::require_isolated_mutable_fixture("script_file_inputs::fixture");
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let library = root.path().join("library");
    let marker_dir = root.path().join("marker");
    for dir in [
        &global,
        &repo.join(".orbit/resources/scripts"),
        &repo.join("site/dist/sub"),
        &repo.join("docs"),
        &library,
        &marker_dir,
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(repo.join("docs/brief.txt"), "brief\n").unwrap();
    std::fs::write(repo.join("site/dist/index.html"), "<p>hi</p>\n").unwrap();
    std::fs::write(repo.join("site/dist/sub/a.txt"), "a\n").unwrap();
    std::fs::write(repo.join(".orbit/secret"), "state\n").unwrap();
    std::fs::write(library.join("ref.md"), "# ref\n").unwrap();
    let library = library.canonicalize().unwrap();
    let ran_marker = marker_dir.join("ran");
    std::fs::write(
        repo.join("check.sh"),
        format!(
            r#"#!/bin/sh
# `:` is a special builtin: a failed redirect exits the shell, so probes use `true`.
true > '{marker}' 2>/dev/null
in="$ORBIT_SCRIPT_IN"
brief=$(cat "$in/brief.txt")
page=$(cat "$in/dist/index.html")
nested=$(cat "$in/dist/sub/a.txt")
ref=$(cat "$in/ref.md")
if echo x > "$in/brief.txt" 2>/dev/null; then file_write=ok; else file_write=refused; fi
if echo x > "$in/dist/new.txt" 2>/dev/null; then dir_write=ok; else dir_write=refused; fi
if rm -f "$in/dist/index.html" 2>/dev/null && [ ! -e "$in/dist/index.html" ]; then remove=ok; else remove=refused; fi
if true > "$in/extra" 2>/dev/null; then in_write=ok; else in_write=refused; fi
printf '{{"brief":"%s","page":"%s","nested":"%s","ref":"%s","file_write":"%s","dir_write":"%s","remove":"%s","in_write":"%s"}}' \
  "$brief" "$page" "$nested" "$ref" "$file_write" "$dir_write" "$remove" "$in_write"
"#,
            marker = ran_marker.display()
        ),
    )
    .unwrap();
    std::fs::write(
        repo.join(".orbit/resources/scripts/check.yaml"),
        format!(
            "schemaVersion: 1\nkind: Script\nmetadata:\n  name: check\nspec:\n  \
             entry: check.sh\n  interpreter: [sh]\n  files:\n    inputs:\n{inputs_yaml}  \
             requests:\n    read_roots: [{}]\n",
            library.display()
        ),
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    runtime
        .grant_script(
            "check",
            &orbit_core::runtime::scripts::ScriptGrantOptions {
                read_roots: vec![library.clone()],
                // The marker dir lies under the bulk temp dir, which the
                // sandbox's private `/dev` hides, so it is bound writable
                // explicitly; the entry's marker write needs it.
                write_roots: vec![marker_dir.clone()],
                ..Default::default()
            },
        )
        .expect("grant the fixture script");
    Fixture {
        _root: root,
        runtime,
        repo,
        library,
        ran_marker,
    }
}

const ALL_INPUTS: &str = "      - {from: \"{{ input.brief }}\", as: brief.txt}\n      \
                          - {from: site/dist, as: dist}\n      \
                          - {from: \"{{ input.library }}/ref.md\", as: ref.md}\n";

fn request(input: Value) -> ScriptStepRequest {
    ScriptStepRequest {
        run_id: "run-inputs".to_string(),
        step_id: "check".to_string(),
        script: "check".to_string(),
        input,
        workspace_path: None,
        timeout_seconds: Some(60),
    }
}

fn physical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

/// The digests recorded on the attempt are the inputs' own: a file's bytes,
/// and a directory's tree digest, which moves when a nested file changes.
fn assert_recorded_inputs(fixture: &Fixture, record: &ScriptExecutionRecord) {
    let names: Vec<_> = record
        .input_files
        .iter()
        .map(|input| input.name.as_str())
        .collect();
    assert_eq!(names, ["brief.txt", "dist", "ref.md"], "{record:?}");
    let brief = &record.input_files[0];
    assert_eq!(brief.source, physical(&fixture.repo.join("docs/brief.txt")));
    assert_eq!(brief.kind, "file");
    assert_eq!(brief.bytes, 6);
    assert_eq!(brief.sha256, script_sha256_hex(b"brief\n"));
    let dist = &record.input_files[1];
    assert_eq!(dist.source, physical(&fixture.repo.join("site/dist")));
    assert_eq!(dist.kind, "directory");
    assert_eq!(dist.sha256.len(), 64);
    let reference = &record.input_files[2];
    assert_eq!(reference.source, physical(&fixture.library.join("ref.md")));
    assert_eq!(reference.sha256, script_sha256_hex(b"# ref\n"));
}

#[test]
fn declared_inputs_are_read_only_at_in_and_recorded_with_their_digests() {
    if !super::dispatch_admission::isolated(
        "script_file_inputs::declared_inputs_are_read_only_at_in_and_recorded_with_their_digests",
    ) {
        return;
    }
    let fixture = fixture(ALL_INPUTS);
    let input = json!({
        "brief": "docs/brief.txt",
        "library": fixture.library.display().to_string(),
    });
    let run = RuntimeHost::run_script_step(&fixture.runtime, &request(input.clone()));
    let ran = match run {
        ScriptStepRun::Refused {
            code,
            message,
            record,
        } if code == script_refusal::SANDBOX_UNAVAILABLE => {
            // Input admission precedes the profile probe, so the record
            // carries the inputs even where the sandbox cannot start.
            assert_recorded_inputs(&fixture, &record);
            assert!(!fixture.ran_marker.exists(), "nothing ran");
            orbit_exec::report_bwrap_deferral(
                "declared_inputs_are_read_only_at_in_and_recorded_with_their_digests",
                &message,
            );
            return;
        }
        ScriptStepRun::Refused { code, message, .. } => {
            panic!("unexpected refusal {code}: {message}")
        }
        ScriptStepRun::Ran(ran) => ran,
    };
    assert_eq!(
        ran.end,
        ScriptStepEnd::Exited(0),
        "{}",
        String::from_utf8_lossy(&ran.log)
    );
    let output: Value = serde_json::from_slice(&ran.stdout).unwrap();
    assert_eq!(
        output,
        json!({
            "brief": "brief",
            "page": "<p>hi</p>",
            "nested": "a",
            "ref": "# ref",
            "file_write": "refused",
            "dir_write": "refused",
            "remove": "refused",
            "in_write": "refused",
        }),
        "every input is visible at in/<as> and none of them, nor in/, is writable"
    );
    assert_recorded_inputs(&fixture, &ran.record);
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("docs/brief.txt")).unwrap(),
        "brief\n"
    );
    assert!(fixture.repo.join("site/dist/index.html").exists());

    // A nested change moves the directory input's recorded digest.
    let first = ran.record.input_files[1].sha256.clone();
    std::fs::write(fixture.repo.join("site/dist/sub/a.txt"), "b\n").unwrap();
    let ScriptStepRun::Ran(again) = RuntimeHost::run_script_step(&fixture.runtime, &request(input))
    else {
        panic!("second attempt refused");
    };
    assert_ne!(again.record.input_files[1].sha256, first);
}

#[test]
fn a_missing_or_out_of_bounds_input_refuses_before_spawn_naming_it() {
    if !super::dispatch_admission::isolated(
        "script_file_inputs::a_missing_or_out_of_bounds_input_refuses_before_spawn_naming_it",
    ) {
        return;
    }
    let fixture = fixture(ALL_INPUTS);
    let outside = fixture._root.path().join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();
    std::fs::create_dir_all(fixture.repo.join("docs/links")).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.repo.join("docs/links/escape.txt")).unwrap();
    let library = fixture.library.display().to_string();
    for (brief, expected) in [
        ("docs/missing.txt", "is missing"),
        ("../outside.txt", "outside the cwd tree"),
        (outside.to_str().unwrap(), "outside the cwd tree"),
        ("docs/links/escape.txt", "outside the cwd tree"),
        (".orbit/secret", "overlaps protected state"),
        (".", "overlaps protected state"),
    ] {
        let run = RuntimeHost::run_script_step(
            &fixture.runtime,
            &request(json!({"brief": brief, "library": library})),
        );
        let ScriptStepRun::Refused {
            code,
            message,
            record,
        } = run
        else {
            panic!("input {brief} must refuse the step");
        };
        assert_eq!(
            code,
            script_refusal::FILE_INPUT_UNAVAILABLE,
            "{brief}: {message}"
        );
        assert!(
            message.contains("`brief.txt`") && message.contains(expected),
            "{brief}: the refusal names the input and why: {message}"
        );
        assert!(
            record.argv.is_empty() && record.profile.is_none(),
            "{record:?}"
        );
        assert!(!fixture.ran_marker.exists(), "{brief}: nothing ran");
    }
}

#[test]
fn an_input_from_an_ungranted_root_refuses_naming_it() {
    if !super::dispatch_admission::isolated(
        "script_file_inputs::an_input_from_an_ungranted_root_refuses_naming_it",
    ) {
        return;
    }
    let fixture = fixture("      - {from: \"{{ input.path }}\", as: elsewhere}\n");
    let elsewhere = fixture._root.path().join("home");
    let run = RuntimeHost::run_script_step(
        &fixture.runtime,
        &request(json!({"path": elsewhere.display().to_string()})),
    );
    let ScriptStepRun::Refused { code, message, .. } = run else {
        panic!("an ungranted root must refuse");
    };
    assert_eq!(code, script_refusal::FILE_INPUT_UNAVAILABLE, "{message}");
    assert!(message.contains("`elsewhere`"), "{message}");
    assert!(!fixture.ran_marker.exists());
}
