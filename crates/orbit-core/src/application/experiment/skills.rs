//! Repository skill trees, read only from the delivery's pinned Git base.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    ExperimentSkillBundle, ExperimentSkillFile, experiment_skill_provider_supported,
};

use super::definition::{ExperimentOverlay, OverlayRole, SkillsOverlay};
use crate::OrbitRuntime;
use crate::application::tag_instructions::LazyRevision;

impl OrbitRuntime {
    pub(super) fn resolve_experiment_skills(
        &self,
        skills: &SkillsOverlay,
        revision: &mut LazyRevision<'_>,
    ) -> Result<Vec<ExperimentSkillBundle>, OrbitError> {
        let revision = revision.get().map_err(OrbitError::InvalidInput)?;
        let git_error = |error: git2::Error| {
            OrbitError::InvalidInput(format!("cannot read skill bundle revision: {error}"))
        };
        let repo = git2::Repository::open(&self.paths().repo_root).map_err(git_error)?;
        let tree = repo
            .revparse_single(&revision)
            .and_then(|object| object.peel_to_tree())
            .map_err(git_error)?;
        let mut bundles = Vec::new();
        for (name, path) in &skills.replace {
            let refusal = |reason: String| {
                OrbitError::InvalidInput(format!(
                    "skill `{name}` bundle `{path}` at {revision}: {reason}"
                ))
            };
            let bundle_tree = tree
                .get_path(std::path::Path::new(path))
                .and_then(|entry| entry.to_object(&repo))
                .and_then(|object| object.peel_to_tree())
                .map_err(|_| refusal("bundle directory is missing from that commit".into()))?;
            let mut bundle = ExperimentSkillBundle {
                name: name.clone(),
                path: path.clone(),
                revision: revision.clone(),
                files: BTreeMap::new(),
                digest: String::new(),
            };
            capture_skill_tree(&repo, &bundle_tree, "", &mut bundle.files, &mut 0, 0)
                .map_err(&refusal)?;
            bundle.digest = bundle.content_digest();
            bundle.verify().map_err(&refusal)?;
            validate_skill_manifest(&bundle).map_err(refusal)?;
            bundles.push(bundle);
        }
        if serde_json::to_vec(&bundles)
            .map_err(|error| OrbitError::Execution(error.to_string()))?
            .len()
            > 256 * 1024
        {
            return Err(OrbitError::InvalidInput(
                "skill bundles exceed the 256 KiB experiment capture budget".into(),
            ));
        }
        Ok(bundles)
    }

    /// Admission is conservative without an implementation-only arm crew
    /// pool: every enabled crew is a possible explicit or purpose selection.
    /// Dispatch checks again because crews can change after experiment start.
    pub(super) fn validate_experiment_skill_providers(
        &self,
        overlay: &ExperimentOverlay,
    ) -> Result<(), OrbitError> {
        let Some(skills) = &overlay.skills else {
            return Ok(());
        };
        let pool = overlay
            .crew
            .as_ref()
            .filter(|_| skills.roles == [OverlayRole::Implement]);
        let crews = match pool {
            Some(pool) => pool
                .pool
                .iter()
                .map(|member| self.resolve_crew_for_task(Some(&member.crew), None))
                .collect::<Result<Vec<_>, _>>()?,
            None => self
                .context
                .settings()
                .crews()
                .values()
                .filter(|crew| crew.enabled)
                .cloned()
                .collect(),
        };
        if crews.is_empty() {
            return Err(OrbitError::InvalidInput(
                "skill overlay has no supported provider candidates".into(),
            ));
        }
        for crew in crews {
            if !experiment_skill_provider_supported(&crew.assignment.provider) {
                return Err(OrbitError::InvalidInput(format!(
                    "skill overlay does not support provider `{}` (crew `{}`); supported: claude on Unix. Use a Claude-only implementation crew pool, or remove the skill overlay",
                    crew.assignment.provider, crew.name
                )));
            }
        }
        Ok(())
    }
}

fn capture_skill_tree(
    repo: &git2::Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    files: &mut BTreeMap<String, ExperimentSkillFile>,
    bytes: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth > 16 {
        return Err("bundle nesting exceeds 16 directories".into());
    }
    for entry in tree {
        let name = entry
            .name()
            .map_err(|_| "bundle has a non-UTF-8 file name")?;
        let path = format!("{prefix}{name}");
        match entry.filemode() {
            0o040000 => {
                let child = repo
                    .find_tree(entry.id())
                    .map_err(|error| error.to_string())?;
                capture_skill_tree(repo, &child, &format!("{path}/"), files, bytes, depth + 1)?;
            }
            0o100644 | 0o100755 => {
                let blob = repo
                    .find_blob(entry.id())
                    .map_err(|error| error.to_string())?;
                if files.len() >= ExperimentSkillBundle::MAX_FILES
                    || blob.size() > ExperimentSkillBundle::MAX_BYTES.saturating_sub(*bytes)
                {
                    return Err("bundle exceeds 256 files or 64 KiB".into());
                }
                *bytes += blob.size();
                files.insert(
                    path,
                    ExperimentSkillFile {
                        bytes: blob.content().to_vec(),
                        executable: entry.filemode() == 0o100755,
                    },
                );
            }
            _ => {
                return Err(format!(
                    "`{path}` is not a regular file or directory (symlinks and submodules are refused)"
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_skill_manifest(bundle: &ExperimentSkillBundle) -> Result<(), String> {
    let manifest = bundle
        .files
        .get("SKILL.md")
        .ok_or("bundle has no SKILL.md")?;
    let text = std::str::from_utf8(&manifest.bytes).map_err(|_| "SKILL.md is not UTF-8")?;
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Err("SKILL.md needs YAML frontmatter".into());
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    if !closed {
        return Err("SKILL.md has unclosed frontmatter".into());
    }
    let meta: serde_yaml::Value = serde_yaml::from_str(&yaml)
        .map_err(|error| format!("invalid SKILL.md frontmatter: {error}"))?;
    if meta["name"].as_str() != Some(bundle.name.as_str())
        || meta["description"]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
        || lines.all(|line| line.trim().is_empty())
    {
        return Err("SKILL.md needs a matching name, a description and instructions".into());
    }
    Ok(())
}
