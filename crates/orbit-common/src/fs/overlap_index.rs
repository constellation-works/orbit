//! Selectors indexed by filesystem anchor so an overlap query is a prefix
//! lookup rather than a scan.
//!
//! [`overlaps`](super::selector::overlaps) answers one pair. A scheduler that asks "which held selectors
//! does this requested selector overlap" for every requested selector against
//! every held one re-parses both sides per pair and does `requested × held`
//! comparisons. Two selectors overlap only when they share an anchor or one
//! anchor is a `/`-boundary prefix of the other, so the held side can be keyed
//! by normalized anchor and a query touches exactly: the requested anchor, each
//! of its ancestors, and — only when the requested selector can contain
//! descendants — the contiguous key range beneath it.
//!
//! Every candidate the prefix walk produces is re-checked with
//! [`OverlapScope::overlaps`], so the answers are those of
//! [`overlaps`](super::selector::overlaps) by construction rather than by a parallel reimplementation of its rules.

use std::collections::BTreeMap;
use std::ops::Bound;

use super::selector::OverlapScope;

/// One indexed selector and the value it carries.
#[derive(Debug, Clone)]
struct OverlapEntry<V> {
    selector: String,
    scope: OverlapScope,
    value: V,
}

/// Selectors keyed by anchor for prefix-lookup overlap queries.
///
/// `V` is whatever the caller needs back alongside the overlapping selector —
/// a holder list, a task ID, a unit for a pure membership test.
#[derive(Debug, Clone)]
pub struct OverlapIndex<V> {
    /// Normalized anchor path -> the entries anchored exactly there. One
    /// anchor can carry several selectors (`file:a.rs`, `symbol:a.rs#f:fn`).
    anchored: BTreeMap<String, Vec<OverlapEntry<V>>>,
    /// Anchor-less selectors (`module:` / `command:`) by trimmed text.
    exact: BTreeMap<String, Vec<OverlapEntry<V>>>,
}

impl<V> Default for OverlapIndex<V> {
    fn default() -> Self {
        Self {
            anchored: BTreeMap::new(),
            exact: BTreeMap::new(),
        }
    }
}

impl<V> OverlapIndex<V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index `selector` with `value`. Returns `false` and indexes nothing when
    /// the selector does not parse: [`overlaps`](super::selector::overlaps)
    /// says such a selector overlaps nothing, and the index agrees by never
    /// producing it.
    pub fn insert(&mut self, selector: &str, value: V) -> bool {
        let Some(scope) = OverlapScope::parse(selector) else {
            return false;
        };
        let bucket = match &scope {
            OverlapScope::Anchored { path, .. } => self.anchored.entry(path.clone()),
            OverlapScope::Exact(text) => self.exact.entry(text.clone()),
        };
        bucket.or_default().push(OverlapEntry {
            selector: selector.to_string(),
            scope,
            value,
        });
        true
    }

    pub fn is_empty(&self) -> bool {
        self.anchored.is_empty() && self.exact.is_empty()
    }

    /// Every indexed selector overlapping `selector`, with its value, in
    /// anchor order. Empty when `selector` does not parse.
    pub fn overlapping<'a>(&'a self, selector: &str) -> Vec<(&'a str, &'a V)> {
        let Some(scope) = OverlapScope::parse(selector) else {
            return Vec::new();
        };
        let candidates: Vec<&OverlapEntry<V>> = match &scope {
            OverlapScope::Exact(text) => self.exact.get(text).into_iter().flatten().collect(),
            OverlapScope::Anchored {
                path,
                contains_descendants,
            } => {
                let mut candidates = Vec::new();
                // Ancestors: every `/`-boundary prefix of the anchor, which is
                // exactly the set an indexed `dir:` or legacy path could be
                // anchored at and still contain this one.
                for (offset, byte) in path.bytes().enumerate() {
                    if byte == b'/' && offset > 0 {
                        candidates.extend(self.anchored.get(&path[..offset]).into_iter().flatten());
                    }
                }
                candidates.extend(self.anchored.get(path.as_str()).into_iter().flatten());
                // Descendants: keys starting with `anchor/` are contiguous in
                // the map, and only a selector that contains descendants can
                // overlap them.
                if *contains_descendants {
                    let prefix = format!("{path}/");
                    candidates.extend(
                        self.anchored
                            .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
                            .take_while(|(key, _)| key.starts_with(&prefix))
                            .flat_map(|(_, entries)| entries),
                    );
                }
                candidates
            }
        };
        candidates
            .into_iter()
            .filter(|entry| scope.overlaps(&entry.scope))
            .map(|entry| (entry.selector.as_str(), &entry.value))
            .collect()
    }
}
