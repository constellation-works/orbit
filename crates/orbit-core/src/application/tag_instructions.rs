//! Per-tag instructions: resolving what an activity receives for its task's
//! tags, and what an owner captures for a claimed leaf.
//!
//! The text has exactly two sources: Orbit's shipped defaults and repository
//! files read from a commit, as `[workflow.tag_instructions]` maps them. The
//! commit is the delivery's base, never its worktree, so a delivery cannot
//! change the instructions it runs under, and nothing on the task (its
//! description, comments or tags) supplies text. A tag only selects.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_config::TagInstructionSetting;
use orbit_types::workflow::{
    CapturedTagInstructions, TAG_INSTRUCTIONS_SCHEMA_VERSION, TagInstruction, TagInstructionError,
    TagInstructionRefusal, TagInstructionRole, TagInstructionSource, select_tag_instructions,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::assets::DEFAULT_TAG_INSTRUCTIONS;
use crate::runtime::run_input::singular_task_id_from_input;

/// Most instruction text one claim receipt captures. An entry past it is
/// captured as a refusal, so the leaf refuses a task it applies to rather
/// than running without it.
const MAX_CAPTURED_TAG_INSTRUCTION_BYTES: usize = 64 * 1024;

/// Why an activity's tag instructions could not be resolved.
#[derive(Debug)]
pub(crate) enum TagInstructionFailure {
    /// The task or run record could not be read.
    Store(OrbitError),
    /// A mapped instruction was refused.
    Refused(TagInstructionError),
}

impl OrbitRuntime {
    /// The tag instructions `activity_name` of `run_id` receives for the task
    /// `input` names, in application order. A claimed leaf answers from its
    /// receipt's capture; every other run resolves this host's mapping, with
    /// workspace files read at the run's base revision.
    pub(crate) fn tag_instructions_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<TagInstruction>, TagInstructionFailure> {
        let Some(role) = TagInstructionRole::for_activity(activity_name) else {
            return Ok(Vec::new());
        };
        let Some(task_id) = singular_task_id_from_input(input) else {
            return Ok(Vec::new());
        };
        let tags = self
            .get_task(task_id)
            .map_err(TagInstructionFailure::Store)?
            .tags;
        if let Some(captured) = self.claimed_tag_instructions() {
            return captured
                .select(&tags, role)
                .map_err(TagInstructionFailure::Refused);
        }
        let mut revision = LazyRevision::new(|| self.run_base_revision(run_id));
        select_tag_instructions(&tags, role, |tag| {
            self.resolve_tag_instruction(tag, role, &mut revision)
        })
        .map_err(TagInstructionFailure::Refused)
    }

    /// Every instruction this host applies, with workspace files read at the
    /// tip of `base_branch`: what an owner captures in a claim's receipt.
    pub(crate) fn captured_tag_instructions(&self, base_branch: &str) -> CapturedTagInstructions {
        let mut pairs = DEFAULT_TAG_INSTRUCTIONS
            .iter()
            .map(|(tag, role, _)| ((*tag).to_string(), *role))
            .chain(
                self.tag_instruction_settings()
                    .entries()
                    .map(|(tag, role, _)| (tag.to_string(), role)),
            )
            .collect::<Vec<_>>();
        pairs.sort();
        pairs.dedup();

        let mut revision = LazyRevision::new(|| self.branch_revision(base_branch));
        let mut captured = CapturedTagInstructions {
            schema_version: TAG_INSTRUCTIONS_SCHEMA_VERSION,
            revision: None,
            instructions: Vec::new(),
            refusals: Vec::new(),
        };
        let mut bytes = 0;
        for (tag, role) in pairs {
            match self.resolve_tag_instruction(&tag, role, &mut revision) {
                Ok(Some(instruction))
                    if bytes + instruction.text.len() > MAX_CAPTURED_TAG_INSTRUCTION_BYTES =>
                {
                    captured.refusals.push(TagInstructionRefusal {
                        tag,
                        role,
                        error: format!(
                            "tag_instructions_too_large: the owner's tag instructions exceed the \
                             {MAX_CAPTURED_TAG_INSTRUCTION_BYTES}-byte claim capture limit"
                        ),
                    });
                }
                Ok(Some(instruction)) => {
                    bytes += instruction.text.len();
                    captured.instructions.push(instruction);
                }
                Ok(None) => {}
                Err(error) => captured.refusals.push(TagInstructionRefusal {
                    tag,
                    role,
                    error: error.to_string(),
                }),
            }
        }
        captured.revision = revision.resolved();
        captured
    }

    /// The instruction `tag` (normalized) gives `role` on this host: the
    /// mapped workspace file, nothing when the mapping is `false`, or else
    /// Orbit's shipped default.
    fn resolve_tag_instruction(
        &self,
        tag: &str,
        role: TagInstructionRole,
        revision: &mut LazyRevision<'_>,
    ) -> Result<Option<TagInstruction>, TagInstructionError> {
        match self.tag_instruction_settings().setting(tag, role) {
            Some(TagInstructionSetting::Disabled) => Ok(None),
            Some(TagInstructionSetting::File(path)) => {
                let unreadable = |reason: String| TagInstructionError::Unreadable {
                    tag: tag.to_string(),
                    role: role.as_str(),
                    origin: path.clone(),
                    reason,
                };
                let revision = revision.get().map_err(unreadable)?;
                let bytes = read_blob_at(&self.paths().repo_root, &revision, path)
                    .map_err(|reason| unreadable(format!("at {revision}: {reason}")))?;
                TagInstruction::admit(
                    tag,
                    role,
                    TagInstructionSource::Workspace {
                        path: path.clone(),
                        revision,
                    },
                    path,
                    &bytes,
                )
                .map(Some)
            }
            None => DEFAULT_TAG_INSTRUCTIONS
                .iter()
                .find(|(default_tag, default_role, _)| *default_tag == tag && *default_role == role)
                .map(|(_, _, text)| {
                    TagInstruction::admit(
                        tag,
                        role,
                        TagInstructionSource::Default,
                        &format!("Orbit default {tag}/{}", role.as_str()),
                        text.as_bytes(),
                    )
                })
                .transpose(),
        }
    }

    /// The receipt's capture when this process executes a claimed leaf. A
    /// leaf whose admission is unreadable, or whose older owner captured
    /// nothing, resolves on this replica instead and says so.
    fn claimed_tag_instructions(&self) -> Option<CapturedTagInstructions> {
        self.worker_invocation()?;
        let captured = self.current_claimed_leaf().map(|leaf| {
            leaf.admission
                .receipt
                .and_then(|receipt| receipt.tag_instructions)
        });
        match captured {
            Ok(Some(captured)) => Some(captured),
            missing => {
                let reason = match missing {
                    Err(error) => format!("its admission could not be read: {error}"),
                    _ => "its admission captured none (an older owner)".to_string(),
                };
                tracing::warn!(
                    "claimed leaf applies this replica's `workflow.tag_instructions` and files, \
                     which may differ from the owner's: {reason}"
                );
                None
            }
        }
    }

    /// The commit the run's worktree was cut from, as its `worktree` step
    /// recorded it, or else this checkout's `HEAD` commit.
    fn run_base_revision(&self, run_id: &str) -> Result<String, String> {
        let recorded = self
            .read_run_state(run_id)
            .ok()
            .flatten()
            .and_then(|state| {
                state
                    .pipeline
                    .get("worktree")
                    .and_then(|worktree| worktree.get("base_sha"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        match recorded {
            Some(sha) => commit_at(&self.paths().repo_root, &sha),
            None => commit_at(&self.paths().repo_root, "HEAD"),
        }
    }

    /// The tip of `branch`, locally or as last fetched, or else `HEAD`.
    fn branch_revision(&self, branch: &str) -> Result<String, String> {
        let root = &self.paths().repo_root;
        [
            format!("refs/heads/{branch}"),
            format!("refs/remotes/origin/{branch}"),
        ]
        .iter()
        .find_map(|reference| commit_at(root, reference).ok())
        .map_or_else(|| commit_at(root, "HEAD"), Ok)
    }
}

/// A revision looked up at most once, and only when a workspace file needs
/// it, so a task with only default instructions never touches Git.
struct LazyRevision<'a> {
    resolve: Box<dyn FnMut() -> Result<String, String> + 'a>,
    value: Option<Result<String, String>>,
}

impl<'a> LazyRevision<'a> {
    fn new(resolve: impl FnMut() -> Result<String, String> + 'a) -> Self {
        Self {
            resolve: Box::new(resolve),
            value: None,
        }
    }

    fn get(&mut self) -> Result<String, String> {
        if self.value.is_none() {
            self.value = Some((self.resolve)());
        }
        self.value
            .clone()
            .unwrap_or_else(|| Err("no revision".to_string()))
    }

    fn resolved(&self) -> Option<String> {
        self.value.clone().and_then(Result::ok)
    }
}

/// The full commit id `revision` names in the repository at `root`.
fn commit_at(root: &Path, revision: &str) -> Result<String, String> {
    let repo = git2::Repository::open(root)
        .map_err(|error| format!("the repository cannot be opened: {}", error.message()))?;
    repo.revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|error| {
            format!(
                "revision {revision} cannot be resolved: {}",
                error.message()
            )
        })
}

/// The bytes of `path` in the tree of commit `revision`. The admission, not
/// this read, enforces the size cap, so an oversized file is refused by name.
fn read_blob_at(root: &Path, revision: &str, path: &str) -> Result<Vec<u8>, String> {
    let repo = git2::Repository::open(root)
        .map_err(|error| format!("the repository cannot be opened: {}", error.message()))?;
    let tree = repo
        .revparse_single(revision)
        .and_then(|object| object.peel_to_tree())
        .map_err(|error| format!("revision cannot be read: {}", error.message()))?;
    let entry = tree
        .get_path(Path::new(path))
        .map_err(|_| "the file is not in that commit".to_string())?;
    let blob = entry
        .to_object(&repo)
        .and_then(|object| object.peel_to_blob())
        .map_err(|_| "the path is not a file".to_string())?;
    Ok(blob.content().to_vec())
}
