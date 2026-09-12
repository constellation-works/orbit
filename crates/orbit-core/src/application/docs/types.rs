use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

const DOC_TYPES: &[&str] = &["design", "pattern", "context", "glossary", "runbook"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DocType {
    Design,
    Pattern,
    Context,
    Glossary,
    Runbook,
}

impl DocType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Pattern => "pattern",
            Self::Context => "context",
            Self::Glossary => "glossary",
            Self::Runbook => "runbook",
        }
    }
}

impl fmt::Display for DocType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DocType {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim() {
            "design" => Ok(Self::Design),
            "pattern" => Ok(Self::Pattern),
            "context" => Ok(Self::Context),
            "glossary" => Ok(Self::Glossary),
            "runbook" => Ok(Self::Runbook),
            other => Err(format!(
                "invalid doc type `{other}`; expected one of: {}",
                DOC_TYPES.join(", ")
            )),
        }
    }
}

impl Serialize for DocType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DocType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactRef {
    Task(String),
    Friction(String),
    Adr(String),
}

impl ArtifactRef {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Task(value) | Self::Friction(value) | Self::Adr(value) => value,
        }
    }
}

impl Serialize for ArtifactRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}
// Deserialize impl for ArtifactRef lives in artifact_ref.rs (calls parse_artifact_ref)

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocFrontmatter {
    #[serde(rename = "type")]
    pub doc_type: DocType,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_features: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_artifacts: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocRecord {
    pub path: String,
    #[serde(flatten)]
    pub frontmatter: DocFrontmatter,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocShow {
    pub path: String,
    pub frontmatter: DocFrontmatter,
    pub body: String,
}

/// Related-doc projection emitted by `task show --with-context`.
///
/// JSON schema:
/// `{"path": string, "type": string, "summary": string, "excerpt": string, "matched_by": string[]}`.
/// The `type` value is one of `design`, `pattern`, `context`, `glossary`, or
/// `runbook`. `matched_by` contains stable `path:<glob>` and `feature:<slug>`
/// markers explaining why the doc was selected.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskRelatedDoc {
    pub path: String,
    #[serde(rename = "type")]
    pub doc_type: DocType,
    pub summary: String,
    pub excerpt: String,
    pub matched_by: Vec<String>,
}

/// How much of the docs corpus has an embedding row, from the last time the
/// vector index was consulted. `total_sources` is the live corpus as
/// discovered by [`walk_docs_roots`](super::walk_docs_roots); `embedded_sources`
/// counts only the ones among them with a live `doc` embedding row — a stale
/// row for a since-deleted doc is not counted as coverage [ORB-12259].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocsEmbeddingCoverage {
    pub total_sources: usize,
    pub embedded_sources: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocAddOutcome {
    pub path: String,
    pub added: bool,
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocMigrationReport {
    pub dry_run: bool,
    pub changed: Vec<DocMigrationChange>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DocMigrationChange {
    pub path: String,
    pub diff: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawDocFrontmatter {
    #[serde(rename = "type")]
    pub(crate) doc_type: Option<DocType>,
    pub(crate) summary: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) paths: Vec<String>,
    #[serde(default)]
    pub(crate) related_features: Vec<String>,
    #[serde(default)]
    pub(crate) related_artifacts: Vec<ArtifactRef>,
}

/// One doc as produced by a single walk: its record plus the body parsed out
/// of the same read. Callers that need the text — search scoring, embeddings,
/// related-doc excerpts — consume this instead of re-reading the file.
#[derive(Debug)]
pub(super) struct WalkedDoc {
    pub(crate) record: DocRecord,
    pub(crate) body: String,
}

#[derive(Debug)]
pub(super) struct ParsedDoc {
    pub(crate) frontmatter: DocFrontmatter,
    pub(crate) body: String,
}

#[derive(Debug)]
pub(super) struct FrontmatterBlock<'a> {
    pub(crate) raw: &'a str,
    pub(crate) body: &'a str,
}
