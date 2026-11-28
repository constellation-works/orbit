---
title: Orbit Core — Design
owner: codex
created_date: 2026-11-28
updated_date: 2026-11-28
status: Draft
feature: orbit-core
doc_role: design
type: design
summary: "Index of the orbit-core mechanism sections: the store access boundary and the application-internal layering order"
tags: [orbit-core, orbit-cmd, architecture]
paths: ["crates/orbit-core/**", "crates/orbit-runtime/**"]
related_features: [orbit-core]
---

# Orbit Core — Design

The orbit-core mechanisms were written into [1_overview.md](./1_overview.md) before the
STD-06@5 scaffold existed, and inbound links still address them there. This doc names
where each one lives; a later change that moves a section here fixes its inbound links in
the same change. Crate-boundary decisions are in [4_decisions.md](./4_decisions.md), and
the repository-wide layering rules are in [`ARCHITECTURE.md`](../../../ARCHITECTURE.md).

## 1. Directional boundaries

`runtime <- application <- adapter`, with `bootstrap` and composition outside them, is
described in the opening of [1_overview.md](./1_overview.md) and its
[§2 Core Concepts](./1_overview.md#2-core-concepts).

## 2. Store access boundary

Core reaches `orbit-store` through the concrete stores and repositories held by
`OrbitStores`. See [overview §4](./1_overview.md#4-store-access-boundary) for
the current access boundary.

## 3. Application-internal layering

The dependency order among the `application/` use-case modules, the shared `contracts`
module and the guard that keeps cut edges from regrowing is
[1_overview.md §5](./1_overview.md#5-application-internal-layering).

## 4. Concerns & Honest Limitations

- The mechanism text lives in the overview, so the overview is much longer than its role
  suggests. Moving it is deferred to keep inbound section links stable.

## Task References

- None cited here; the cited sections carry their own task references.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
