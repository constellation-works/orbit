use orbit_common::OrbitError;
use orbit_types::identity::validate_machine_id;
use orbit_types::policy::compile_glob_regex;
use orbit_types::task::validate_relative_artifact_path;
use orbit_types::workflow::{
    EvidenceHostOs, HostEvidenceRule, HostSandboxCommand, ReviewEvidenceKind,
};

use super::keys::{REVIEW_BEFORE_LANDING_HOSTS_KEY, REVIEW_HOST_EVIDENCE_KEY};

/// Trim, validate and dedupe the listed machine ids. A label that could never
/// match a machine fails the load rather than silently selecting nobody.
pub(crate) fn before_landing_hosts(
    raw: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, OrbitError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut hosts = Vec::with_capacity(raw.len());
    for (index, host) in raw.iter().enumerate() {
        let host = host.trim();
        validate_machine_id(host).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{REVIEW_BEFORE_LANDING_HOSTS_KEY}[{index}] `{host}` is not a machine id: {error}"
            ))
        })?;
        if !hosts.iter().any(|known| known == host) {
            hosts.push(host.to_string());
        }
    }
    Ok(Some(hosts))
}

/// Refuse a rule Orbit could never fulfil, so a misconfiguration fails at
/// load instead of holding every matching review forever.
pub(super) fn host_evidence_rules(
    rules: Vec<HostEvidenceRule>,
) -> Result<Vec<HostEvidenceRule>, OrbitError> {
    let invalid = |index: usize, why: String| {
        OrbitError::InvalidInput(format!("{REVIEW_HOST_EVIDENCE_KEY}[{index}] {why}"))
    };
    let mut artifacts = std::collections::BTreeSet::new();
    rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| {
            let rule = HostEvidenceRule {
                name: rule.name.trim().to_string(),
                command: rule.command.trim().to_string(),
                artifact: rule.artifact.trim().to_string(),
                hosted_check: rule
                    .hosted_check
                    .as_deref()
                    .map(|check| check.trim().to_string()),
                paths: rule
                    .paths
                    .iter()
                    .map(|path| path.trim().to_string())
                    .collect(),
                ..rule
            };
            if !HostEvidenceRule::admits_kind(rule.kind) {
                return Err(invalid(
                    index,
                    "has kind outside codeql and host_sandbox_test".to_string(),
                ));
            }
            if rule.name.is_empty() || rule.command.is_empty() {
                return Err(invalid(index, "needs a name and a command".to_string()));
            }
            if rule.paths.is_empty() {
                return Err(invalid(index, "needs at least one path glob".to_string()));
            }
            for pattern in &rule.paths {
                compile_glob_regex(pattern).map_err(|error| {
                    invalid(index, format!("has invalid path glob `{pattern}`: {error}"))
                })?;
            }
            validate_relative_artifact_path(&rule.artifact)
                .map_err(|error| invalid(index, format!("has an invalid artifact: {error}")))?;
            if !rule.artifact.ends_with(".json") || rule.artifact.starts_with("./") {
                return Err(invalid(
                    index,
                    format!("artifact `{}` must be a relative .json path", rule.artifact),
                ));
            }
            if !artifacts.insert(rule.artifact.clone()) {
                return Err(invalid(
                    index,
                    format!("repeats artifact `{}`", rule.artifact),
                ));
            }
            if let Some(check) = &rule.hosted_check {
                if rule.kind != ReviewEvidenceKind::CodeQl || rule.os != EvidenceHostOs::Linux {
                    return Err(invalid(
                        index,
                        "names a hosted_check, which only a linux codeql rule may".to_string(),
                    ));
                }
                if check.is_empty() {
                    return Err(invalid(index, "has an empty hosted_check".to_string()));
                }
            }
            if rule.kind == ReviewEvidenceKind::HostSandboxTest {
                HostSandboxCommand::admit(&rule.command, &[])
                    .map_err(|refusal| invalid(index, format!("command: {}", refusal.detail)))?;
            }
            Ok(rule)
        })
        .collect()
}
