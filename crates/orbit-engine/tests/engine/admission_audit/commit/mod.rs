mod candidate_paths;
mod git_ops;
mod test_support;

use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, execute_deterministic_action};
use serde_json::Value;
use std::path::Path;

fn git_commit(host: &impl RuntimeHost, input: &Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(
        host,
        "git_commit",
        &Value::Null,
        input,
        false,
        &Default::default(),
        None,
    )
}
fn git_output(repo: &Path, args: &[&str]) -> Result<String, OrbitError> {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(repo)
        .output()?;
    if !output.status.success() {
        return Err(OrbitError::Execution(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
fn git_success(repo: &Path, args: &[&str]) -> Result<(), OrbitError> {
    git_output(repo, args).map(|_| ())
}
