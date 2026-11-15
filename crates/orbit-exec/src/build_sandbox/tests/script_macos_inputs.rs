//! A private timing fixture drives the public script boundary; it never
//! tests the copy helper directly or needs permission to start a sandbox.

use std::io::Write;
use std::time::Duration;

use crate::build_sandbox::script_macos_inputs::AFTER_INPUT_COPY_CHUNK;
use crate::{
    SCRIPT_SCRATCH_DIRS, ScriptSandboxInput, ScriptSandboxNetwork, ScriptSandboxRequest,
    ScriptSandboxSpec, run_script_sandboxed,
};

/// A file admitted at 8 KiB grows after its first copied chunk. The next
/// read must refuse without writing past a 12 KiB cap or starting the script.
/// Unit admission: deterministic TOCTOU injection for ORB-21492 through
/// `run_script_sandboxed`; the hook is private and absent from shipped builds.
#[test]
fn an_input_growing_during_preparation_cannot_escape_the_scratch_cap() {
    let dir = tempfile::tempdir().expect("fixture");
    let root = dir.path().canonicalize().expect("physical fixture");
    let cwd = root.join("work");
    std::fs::create_dir(&cwd).expect("cwd");
    let scratch = root.join("scratch");
    for name in SCRIPT_SCRATCH_DIRS {
        std::fs::create_dir_all(scratch.join(name)).expect("scratch directory");
    }
    let source = root.join("input");
    std::fs::write(&source, vec![0; 8192]).expect("input");
    let growing = source.clone();
    AFTER_INPUT_COPY_CHUNK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::OpenOptions::new()
                .append(true)
                .open(growing)
                .expect("grow the open input")
                .write_all(&[0; 8192])
                .expect("append");
        }));
    });
    let inputs = [ScriptSandboxInput {
        source: source.clone(),
        name: "data".to_string(),
    }];
    let marker = cwd.join("ran");
    let argv = ["/usr/bin/touch".to_string(), marker.display().to_string()];
    let result = run_script_sandboxed(ScriptSandboxRequest {
        sandbox: ScriptSandboxSpec {
            cwd: &cwd,
            cwd_writable: true,
            scratch: &scratch,
            readable: &[],
            writable: &[],
            masked: &[],
            home: None,
            inputs: &inputs,
            network: ScriptSandboxNetwork::None,
        },
        argv: &argv,
        env: &[],
        launcher: &[],
        launcher_env: &[],
        stdin: Vec::new(),
        timeout: Duration::from_secs(30),
        scratch_cap_bytes: 12288,
    });
    AFTER_INPUT_COPY_CHUNK.with(|hook| {
        hook.take();
    });
    let error = result.expect_err("the growing input must be refused");
    assert!(
        matches!(&error, orbit_common::OrbitError::InvalidInput(message) if message.contains("scratch cap")),
        "{error:?}"
    );
    assert_eq!(
        std::fs::metadata(source).expect("grown input").len(),
        16384,
        "the fixture must grow the source only after copying started"
    );
    assert_eq!(
        std::fs::metadata(scratch.join("in/data"))
            .expect("partial copy")
            .len(),
        8192,
        "the chunk that exceeds the remaining cap must not be written"
    );
    assert!(
        !marker.exists(),
        "the script must never start after a failed copy"
    );
}
