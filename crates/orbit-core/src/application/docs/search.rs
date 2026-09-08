use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::anchor_path;
use orbit_search::{DocEmbeddingSource, DocSearchSource};
use orbit_types::policy::{match_glob, normalize_glob_path};

use super::config::DocsRoot;
use super::frontmatter::read_doc_tolerant;
use super::path_util::{path_to_slash_string, repo_relative_path};
use super::types::{DocRecord, DocShow, TaskRelatedDoc, WalkedDoc};
use super::walk::{expand_root, walk_docs_with_bodies};

const DEFAULT_RELATED_DOC_LIMIT: usize = 5;

pub(super) fn show_doc(
    repo_root: &Path,
    roots: &[DocsRoot],
    requested: &str,
) -> Result<DocShow, OrbitError> {
    let requested_path = Path::new(requested.trim());
    let absolute = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        repo_root.join(requested_path)
    };
    if !absolute.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "docs path does not exist or is not a file: {requested}"
        )));
    }
    if path_is_or_contains_dot_orbit(repo_root, &absolute) {
        return Err(OrbitError::InvalidInput(
            "docs paths under .orbit/ are not indexed by orbit-docs".to_string(),
        ));
    }
    if !path_is_under_configured_roots(repo_root, roots, &absolute)? {
        return Err(OrbitError::InvalidInput(format!(
            "docs path is outside configured [docs].roots: {requested}"
        )));
    }
    let relative = repo_relative_path(repo_root, &absolute)?;
    let parsed = read_doc_tolerant(&relative, &absolute)?;
    Ok(DocShow {
        path: path_to_slash_string(&relative),
        frontmatter: parsed.frontmatter,
        body: parsed.body,
    })
}

fn path_is_or_contains_dot_orbit(repo_root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    relative.components().any(
        |component| matches!(component, std::path::Component::Normal(value) if value == ".orbit"),
    )
}

pub(super) fn path_is_under_configured_roots(
    repo_root: &Path,
    roots: &[DocsRoot],
    path: &Path,
) -> Result<bool, OrbitError> {
    let canonical_path = path
        .canonicalize()
        .map_err(|error| OrbitError::Io(format!("canonicalize {}: {error}", path.display())))?;
    for root in roots {
        for root_path in expand_root(repo_root, &root.path)? {
            if path_is_or_contains_dot_orbit(repo_root, &root_path) {
                continue;
            }
            if root_path.is_file() {
                let canonical_root = root_path.canonicalize().map_err(|error| {
                    OrbitError::Io(format!("canonicalize {}: {error}", root_path.display()))
                })?;
                if canonical_path == canonical_root {
                    return Ok(true);
                }
            } else if root_path.is_dir() {
                let canonical_root = root_path.canonicalize().map_err(|error| {
                    OrbitError::Io(format!("canonicalize {}: {error}", root_path.display()))
                })?;
                if canonical_path.starts_with(canonical_root) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub(super) fn doc_search_source(record: DocRecord, body: String) -> DocSearchSource {
    DocSearchSource {
        path: record.path,
        doc_type: record.frontmatter.doc_type.as_str().to_string(),
        summary: record.frontmatter.summary,
        tags: record.frontmatter.tags,
        paths: record.frontmatter.paths,
        related_features: record.frontmatter.related_features,
        related_artifacts: record
            .frontmatter
            .related_artifacts
            .into_iter()
            .map(|artifact| artifact.as_str().to_string())
            .collect(),
        body,
    }
}

pub(super) fn doc_embedding_sources(
    repo_root: &Path,
    roots: &[DocsRoot],
) -> Result<Vec<DocEmbeddingSource>, OrbitError> {
    Ok(walk_docs_with_bodies(repo_root, roots)?
        .into_iter()
        .map(|doc| DocEmbeddingSource {
            path: doc.record.path,
            title: doc.record.frontmatter.summary,
            tags: doc.record.frontmatter.tags,
            body: doc.body,
        })
        .collect())
}

#[derive(Debug)]
struct RelatedDocCandidate {
    record: DocRecord,
    body: String,
    score: usize,
    matched_by: BTreeSet<String>,
}

pub(super) fn related_docs_for_context(
    repo_root: &Path,
    roots: &[DocsRoot],
    context_files: &[String],
    related_features: &[String],
    limit: Option<usize>,
) -> Result<Vec<TaskRelatedDoc>, OrbitError> {
    let limit = limit.unwrap_or(DEFAULT_RELATED_DOC_LIMIT);
    if limit == 0 {
        return Ok(Vec::new());
    }

    let context_paths = context_files
        .iter()
        .filter_map(|selector| context_selector_path(repo_root, selector))
        .collect::<Vec<_>>();
    let features = related_features
        .iter()
        .map(|feature| feature.trim().to_ascii_lowercase())
        .filter(|feature| !feature.is_empty())
        .collect::<BTreeSet<_>>();
    if context_paths.is_empty() && features.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates = BTreeMap::<String, RelatedDocCandidate>::new();
    for WalkedDoc { record, body } in walk_docs_with_bodies(repo_root, roots)? {
        let mut score = 0usize;
        let mut matched_by = BTreeSet::new();

        for glob in &record.frontmatter.paths {
            let Some(normalized_glob) = normalize_doc_path_glob(glob) else {
                continue;
            };
            for context_path in &context_paths {
                if doc_path_glob_matches_context(&normalized_glob, context_path)? {
                    score += 200 + normalized_glob.len();
                    matched_by.insert(format!("path:{glob}"));
                    break;
                }
            }
        }

        for feature in &record.frontmatter.related_features {
            let normalized = feature.trim().to_ascii_lowercase();
            if !normalized.is_empty() && features.contains(&normalized) {
                score += 160 + normalized.len();
                matched_by.insert(format!("feature:{feature}"));
            }
        }

        if score == 0 {
            continue;
        }

        candidates
            .entry(record.path.clone())
            .and_modify(|candidate| {
                candidate.score += score;
                candidate.matched_by.extend(matched_by.iter().cloned());
            })
            .or_insert(RelatedDocCandidate {
                record,
                body,
                score,
                matched_by,
            });
    }

    let mut ranked = candidates.into_values().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.record.path.cmp(&right.record.path))
    });
    ranked.truncate(limit);

    Ok(ranked
        .into_iter()
        .map(|candidate| {
            let excerpt = doc_excerpt(&candidate.body, &candidate.record.frontmatter.summary);
            TaskRelatedDoc {
                path: candidate.record.path,
                doc_type: candidate.record.frontmatter.doc_type,
                summary: candidate.record.frontmatter.summary,
                excerpt,
                matched_by: candidate.matched_by.into_iter().collect(),
            }
        })
        .collect())
}

fn context_selector_path(repo_root: &Path, selector: &str) -> Option<String> {
    let anchor = anchor_path(selector).ok()?;
    let relative = if anchor.is_absolute() {
        anchor.strip_prefix(repo_root).ok()?.to_path_buf()
    } else {
        anchor
    };
    normalize_glob_path(&path_to_slash_string(&relative)).ok()
}

fn normalize_doc_path_glob(glob: &str) -> Option<String> {
    normalize_glob_path(glob).ok()
}

fn doc_path_glob_matches_context(glob: &str, context_path: &str) -> Result<bool, OrbitError> {
    if match_glob(glob, context_path)? {
        return Ok(true);
    }
    let literal_prefix = glob.trim_end_matches('/');
    Ok(!contains_glob_operator(literal_prefix)
        && context_path
            .strip_prefix(literal_prefix)
            .is_some_and(|rest| rest.starts_with('/')))
}

fn contains_glob_operator(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

fn doc_excerpt(body: &str, fallback: &str) -> String {
    for line in body.lines() {
        let trimmed = line
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_matches('`')
            .trim();
        if !trimmed.is_empty() && trimmed != "---" {
            return truncate_excerpt(trimmed);
        }
    }
    truncate_excerpt(fallback)
}

fn truncate_excerpt(value: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 160;
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index == MAX_EXCERPT_CHARS {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}
