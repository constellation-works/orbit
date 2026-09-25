//! Scoreboard files on disk: the summary write path and the validated reads
//! of the model and token scoreboards.

use super::ScoreboardSummary;
use super::overlay::normalize_model_scoreboard;
use super::types::FamilyScoreboard;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text_volatile as write_atomic;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const SUMMARY_FILENAME: &str = "summary.json";

pub(super) const PR_SCOREBOARD_FILENAME: &str = "pr.json";

pub(super) const TOKEN_SCOREBOARD_FILENAME: &str = "tokens.json";

pub fn write_summary(
    scoreboard_dir: &Path,
    summary: &ScoreboardSummary,
) -> Result<std::path::PathBuf, OrbitError> {
    let path = scoreboard_dir.join(SUMMARY_FILENAME);
    let raw = serde_json::to_string_pretty(summary)
        .map_err(|e| OrbitError::Io(format!("serialize summary.json: {e}")))?;
    write_atomic(&path, &format!("{raw}\n"))?;
    Ok(path)
}

pub fn summary_path(scoreboard_dir: &Path) -> std::path::PathBuf {
    scoreboard_dir.join(SUMMARY_FILENAME)
}

pub(super) fn read_model_scoreboard(scoreboard_dir: &Path) -> Result<FamilyScoreboard, OrbitError> {
    let Some(path) = validated_scoreboard_file_path(scoreboard_dir, ScoreboardFile::Pr)? else {
        return Ok(FamilyScoreboard::new());
    };
    let raw = fs::read_to_string(&path)
        .map_err(|e| OrbitError::Io(format!("read {PR_SCOREBOARD_FILENAME}: {e}")))?;
    if raw.trim().is_empty() {
        return Ok(FamilyScoreboard::new());
    }
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| OrbitError::Io(format!("parse {PR_SCOREBOARD_FILENAME}: {e}")))?;
    normalize_model_scoreboard(parsed)
}

/// The fixed snapshot files are selected by the store, never by a caller.
#[derive(Clone, Copy)]
enum ScoreboardFile {
    Pr,
    Tokens,
}

impl ScoreboardFile {
    fn filename(self) -> &'static str {
        match self {
            Self::Pr => PR_SCOREBOARD_FILENAME,
            Self::Tokens => TOKEN_SCOREBOARD_FILENAME,
        }
    }
}

/// Resolve a fixed scoreboard file beneath the selected scoreboard root.
///
/// The root is selected by the workspace configuration, but the file read by
/// this summary is fixed. Canonicalizing the root, checking containment, and
/// rejecting a symlink or non-regular target prevents a path component from
/// redirecting this read to an unrelated file.
fn validated_scoreboard_file_path(
    scoreboard_dir: &Path,
    file: ScoreboardFile,
) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_dir = match fs::canonicalize(scoreboard_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "canonicalize scoreboard directory {}: {error}",
                scoreboard_dir.display()
            )));
        }
    };
    if !canonical_dir.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard path must be a directory: {}",
            scoreboard_dir.display()
        )));
    }

    let candidate = canonical_dir.join(file.filename());
    let metadata = match fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect scoreboard file {}: {error}",
                candidate.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard file must not be a symlink: {}",
            candidate.display()
        )));
    }
    if !metadata.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard file must be a regular file: {}",
            candidate.display()
        )));
    }

    // Read only the canonical file path. This removes aliases and symlink
    // components from the path passed to the filesystem read.
    let path = match fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "canonicalize scoreboard file {}: {error}",
                candidate.display()
            )));
        }
    };
    if !path.starts_with(&canonical_dir) {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard file must remain within the scoreboard directory: {}",
            path.display()
        )));
    }

    Ok(Some(path))
}

pub(super) fn read_token_agents(scoreboard_dir: &Path) -> Result<Vec<TokenAgentEntry>, OrbitError> {
    let Some(path) = validated_scoreboard_file_path(scoreboard_dir, ScoreboardFile::Tokens)? else {
        return Ok(Vec::new());
    };
    let raw =
        fs::read_to_string(&path).map_err(|e| OrbitError::Io(format!("read tokens.json: {e}")))?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed: TokenScoreboardFile = serde_json::from_str(&raw)
        .map_err(|e| OrbitError::Io(format!("parse tokens.json: {e}")))?;
    Ok(parsed.agents)
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokenScoreboardFile {
    #[serde(default)]
    agents: Vec<TokenAgentEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct TokenAgentEntry {
    #[serde(rename = "agent")]
    _agent: String,
    /// Model key used for this token scoreboard row; per-invocation actual execution (from audit/token metrics, not run-level lineup).
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) total_tokens: u64,
    #[serde(default, alias = "output_tokens")]
    pub(super) total_output_tokens: u64,
    #[serde(default)]
    pub(super) total_tool_calls: u64,
}
