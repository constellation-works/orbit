//! Finite supported REST-read commands and their trusted runner bindings.
use super::prerequisite::unreadable;
use orbit_common::OrbitError;
use serde_json::Value;

/// Bind the failed API step to the prerequisite's source, not arbitrary log
/// text from an unrelated job/step. Certify every step the failed job repeats;
/// supported command shapes are REST reads with only isolated runner output.
/// Arbitrary scripts, actions and execution overrides never qualify.
pub(super) fn api_step(
    workflow: &Value,
    prerequisite: &str,
    failed_step: &str,
) -> Result<(), OrbitError> {
    let job = &workflow["jobs"][prerequisite];
    if !matches!(
        job["runs-on"].as_str(),
        Some("ubuntu-latest" | "ubuntu-22.04" | "ubuntu-24.04")
    ) || job.get("container").is_some()
        || job.get("services").is_some()
        || job.get("environment").is_some()
    {
        return Err(unreadable("unsupported prerequisite execution environment"));
    }
    for env in [workflow.get("env"), job.get("env")].into_iter().flatten() {
        safe_environment(env)?;
    }
    let steps = workflow["jobs"][prerequisite]["steps"]
        .as_array()
        .filter(|steps| !steps.is_empty() && steps.len() <= 100)
        .ok_or_else(|| unreadable("prerequisite source steps"))?;
    let matching = steps
        .iter()
        .filter(|step| step["name"].as_str() == Some(failed_step))
        .collect::<Vec<_>>();
    let [_] = matching.as_slice() else {
        return Err(unreadable("failed step has no unique source command"));
    };
    for candidate in steps {
        supported_step(workflow, prerequisite, candidate)?;
    }
    Ok(())
}

pub(super) fn safe_environment(env: &Value) -> Result<(), OrbitError> {
    let env = env
        .as_object()
        .ok_or_else(|| unreadable("ambiguous prerequisite environment"))?;
    for (key, value) in env {
        if matches!(key.as_str(), "GH_TOKEN" | "GITHUB_TOKEN") {
            if !matches!(
                value.as_str(),
                Some("${{ github.token }}" | "${{ secrets.GITHUB_TOKEN }}")
            ) {
                return Err(unreadable("unsupported prerequisite credential source"));
            }
        } else if key == "PR_NUMBER" {
            if value.as_str() != Some("${{ github.event.pull_request.number }}") {
                return Err(unreadable("unsupported prerequisite PR binding"));
            }
        } else if key == "GH_REPO" {
            if value.as_str() != Some("${{ github.repository }}") {
                return Err(unreadable("unsupported prerequisite repository source"));
            }
        } else if key.starts_with("BASH_")
            || key.starts_with("GH_")
            || key.starts_with("GITHUB_")
            || key.starts_with("LD_")
            || key.starts_with("DYLD_")
            || matches!(
                key.as_str(),
                "PATH" | "BASH_ENV" | "ENV" | "SHELLOPTS" | "BASHOPTS" | "HOME" | "XDG_CONFIG_HOME"
            )
        {
            return Err(unreadable("unsupported prerequisite environment override"));
        }
    }
    Ok(())
}

fn supported_step(workflow: &Value, prerequisite: &str, step: &Value) -> Result<(), OrbitError> {
    if let Some(env) = step.get("env") {
        safe_environment(env)?;
    }
    let command = step["run"]
        .as_str()
        .ok_or_else(|| unreadable("failed step has no source command"))?;
    let shell = [
        step.get("shell"),
        workflow["jobs"][prerequisite].pointer("/defaults/run/shell"),
        workflow.pointer("/defaults/run/shell"),
    ]
    .into_iter()
    .flatten()
    .next();
    if step.get("uses").is_some()
        || step.get("working-directory").is_some()
        || workflow["jobs"][prerequisite]
            .pointer("/defaults/run/working-directory")
            .is_some()
        || workflow
            .pointer("/defaults/run/working-directory")
            .is_some()
        || shell.is_some_and(|shell| !matches!(shell.as_str(), Some("bash" | "sh")))
        || !(literal_api_read(command)
            || projected_api_read(command)
            || legacy_api_read(command, workflow, prerequisite, step))
    {
        return Err(unreadable("failed step is not a source-bound API read"));
    }
    Ok(())
}

/// A bounded allowlist rather than shell tokenization: quoting, expansion,
/// comments, redirection and composition all hold. Unknown gh options also
/// hold, including field/input flags that silently change GET to POST.
fn literal_api_read(command: &str) -> bool {
    let command = command.trim_matches([' ', '\t', '\r', '\n']);
    if !command
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b" \t/._-{}=".contains(&byte))
    {
        return false;
    }
    if command.contains(['\n', '\r']) {
        return false;
    }
    let mut words = command.split_ascii_whitespace();
    if words.next() != Some("gh") || words.next() != Some("api") {
        return false;
    }
    let mut endpoint = false;
    let mut method = false;
    while let Some(word) = words.next() {
        match word {
            "--method" | "-X" if !method => {
                if words.next() != Some("GET") {
                    return false;
                }
                method = true;
            }
            "--method=GET" | "-XGET" if !method => method = true,
            _ => {
                // Repository REST paths are supported. In particular, `graphql`
                // defaults to POST even without fields or an explicit method.
                if endpoint
                    || !word.starts_with("repos/")
                    || word.len() <= "repos/".len()
                    || word.contains('=')
                    || word
                        .replace("{owner}", "")
                        .replace("{repo}", "")
                        .contains(['{', '}'])
                {
                    return false;
                }
                endpoint = true;
            }
        }
    }
    endpoint
}

/// An additional single-command form: explicit GET, one single-quoted REST
/// endpoint, gh's in-process JSON projection, and the isolated runner output
/// file. This is an exact command shape, not a shell parser. Only the provider's
/// numeric PR expression can be interpolated; neither quoted argument can
/// escape into shell syntax. No pipeline, action, setup command or arbitrary
/// destination is admitted.
fn projected_api_read(command: &str) -> bool {
    let Some(body) = command
        .trim()
        .strip_prefix("gh api --method GET '")
        .and_then(|body| body.strip_suffix("' >> \"$GITHUB_OUTPUT\""))
    else {
        return false;
    };
    let Some((endpoint, projection)) = body.split_once("' --jq '") else {
        return false;
    };
    // Only a bounded scalar can reach the output file. Arbitrary jq programs
    // can recurse, emit additional output bindings or expose environment data.
    if projection.trim() != STATE_PROJECTION {
        return false;
    }
    let endpoint = endpoint.replace("${{ github.event.pull_request.number }}", "1");
    !endpoint.contains('\'') && literal_api_read(&format!("gh api --method GET {endpoint}"))
}

const STATE_PROJECTION: &str = "if .merged == true then \"state=merged\"\n  elif .state == \"open\" or .state == \"closed\" then \"state=\" + .state\n  else error(\"Unexpected pull request state\") end";

/// The historical composed read is a finite template, not a shell language.
/// Its endpoint uses only runner repository / provider numeric PR bindings;
/// jq produces one of three fixed scalars, consumed by local output and log.
fn legacy_api_read(command: &str, workflow: &Value, job: &str, step: &Value) -> bool {
    const LEGACY: &str = "set -euo pipefail\nstate=\"$(gh api \"repos/${GITHUB_REPOSITORY}/pulls/${PR_NUMBER}\" | jq -er '\n  if .merged == true then \"merged\"\n  elif .state == \"open\" or .state == \"closed\" then .state\n  else error(\"Unexpected pull request state\") end\n')\"\necho \"state=$state\" >> \"$GITHUB_OUTPUT\"\necho \"PR #${PR_NUMBER} live state: $state\"";
    let binding = |key: &str| {
        step["env"]
            .get(key)
            .or_else(|| workflow["jobs"][job]["env"].get(key))
            .or_else(|| workflow["env"].get(key))
            .and_then(Value::as_str)
    };
    command.trim() == LEGACY
        && step["shell"] == "bash"
        && binding("PR_NUMBER") == Some("${{ github.event.pull_request.number }}")
        && matches!(
            binding("GH_TOKEN"),
            Some("${{ github.token }}" | "${{ secrets.GITHUB_TOKEN }}")
        )
}
