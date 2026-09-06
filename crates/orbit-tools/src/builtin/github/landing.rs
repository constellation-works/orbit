//! Bounded provider evidence for an integration-branch commit.
use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
/// Resolve PR identities associated by GitHub with an exact landed commit.
pub fn commit_pull_requests(repository: &str, commit: &str) -> Result<ExecRequest, OrbitError> {
    if repository.split('/').count() != 2
        || repository
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b"/-_.".contains(&b)))
        || !matches!(commit.len(), 40 | 64)
        || !commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(OrbitError::InvalidInput(
            "invalid landing repository or commit".into(),
        ));
    }
    Ok(super::gh_exec_request(
        vec![
            "api".into(),
            format!("repos/{repository}/commits/{commit}/pulls?per_page=100"),
        ],
        None,
        2000,
    ))
}
