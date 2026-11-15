---
type: design
summary: "Glossary: operator attention"
last_validated: 2026-11-11
---

# Glossary: operator attention

Vocabulary specific to `orbit attention`. Standard terms (delta, TTL, fan-out) are excluded
unless the feature gives them a narrower meaning.

| Term | Meaning |
|------|---------|
| **Aggregator** | The outermost caller layer that candidate-reads each destination, merges those items, applies the cursor and the public response limit, and builds unreachable-host items. [attention-federation](../specs/attention-federation.md). |
| **Attention** | The state of waiting on an operator, and the name of the tool (`orbit.attention`) and CLI (`orbit attention`). [1_overview.md](../1_overview.md). |
| **Candidate read** | The aggregator's per-destination retrieval. It returns the post-filter items bounded by the per-workspace cap, not by the public response limit of 100. [attention-federation](../specs/attention-federation.md#candidate-read). |
| **Cursor** | An opaque `atc1.` token holding hashes of the `(id, fingerprint)` pairs a caller has seen. [attention-cursor](../specs/attention-cursor.md). |
| **Degraded** | An envelope entry naming a kind, workspace or host that could not be read, with a code. Never filterable. [attention-item](../specs/attention-item.md#response-envelope). |
| **Fan-out read** | A tool the federated contract allows without a selector, called on every in-scope destination and merged. `orbit.attention` is the only one. [attention-federation](../specs/attention-federation.md#amendment-to-the-federated-mcp-contract). |
| **Fingerprint** | `fp_` plus 16 hex over `{id, severity, per-kind state}`; changes only when an item materially changes. [attention-cursor](../specs/attention-cursor.md#fingerprint). |
| **Folding** | Merging an open decision request into the item of the state it decides. [attention-item](../specs/attention-item.md#kinds-and-sources-of-truth). |
| **Item** | One entry in the feed: a kind, severity, ids, `since`, `reason`, `next` and fingerprint. [attention-item](../specs/attention-item.md). |
| **Kind** | A member of the closed enum of reasons an item exists. [attention-item](../specs/attention-item.md#kinds-and-sources-of-truth). |
| **Next** | The exact tool call and CLI command that resolves an item, with operator-supplied `needs`. [attention-item](../specs/attention-item.md#next). |
| **Task-level kind** | A kind derived from one workspace's task, decision and review records alone. [attention-item](../specs/attention-item.md#kinds-and-sources-of-truth). |
