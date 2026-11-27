mod candidate_paths;
mod git_ops;
mod message_standard;
mod settled_review;
mod test_support;

use orbit_common::OrbitError;
use orbit_engine::test_support::git_commit;
use std::path::Path;

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
