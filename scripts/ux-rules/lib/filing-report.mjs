// Filing report and raw observation archive.
//
// A full UI sweep measures tens of thousands of member observations, far
// more than the filer's script stdout budget. The filing report keeps what
// filing decides on: every failing, error and skipped observation, every
// observation of a defect group that fails anywhere in this sweep or that an
// open owner tracks (from the owner context), every result that declares no
// members, and one compact record per expanded aggregate that did not fail
// (legacy aggregate owners and their pass semantics). Untracked passing
// members are counted, not listed. The complete raw report is kept
// losslessly as gzip parts of at most 1 MiB, which the filer never inflates.
import { createHash } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { gzipSync } from "node:zlib";

export const ARCHIVE_PART_BYTES = 1024 * 1024;
const COMPACT_VALUE_BYTES = 256;

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function groupKeyOf(surface, result) {
  return JSON.stringify([result.surface || surface, result.rule, result.defect.version, result.defect.key]);
}

// Tracked groups from the filer's read-only owner context. Anything else in
// the context is ignored; a malformed context is refused, never guessed at.
export function trackedGroups(context, surface) {
  const tracked = new Set();
  if (context == null) return tracked;
  if (typeof context !== "object" || !Array.isArray(context.groups)) {
    throw new Error("owner context must be an object with a groups array");
  }
  for (const group of context.groups) {
    const version = group?.defect?.version;
    const key = group?.defect?.key;
    if (typeof group?.rule !== "string" || !Number.isInteger(version) || typeof key !== "string") {
      throw new Error("owner context group needs rule and defect {version, key}");
    }
    tracked.add(groupKeyOf(surface, { surface: group.surface, rule: group.rule, defect: { version, key } }));
  }
  return tracked;
}

// Scalars and small values stay; larger ones are named as archived.
export function compactMeasurements(measurements = {}) {
  const kept = {};
  const archived = [];
  for (const [name, value] of Object.entries(measurements)) {
    if (value === null || typeof value !== "object" || JSON.stringify(value).length <= COMPACT_VALUE_BYTES) {
      kept[name] = value;
    } else {
      archived.push(name);
    }
  }
  if (archived.length) kept.archived = archived.sort();
  return kept;
}

// Fixture origins carry ephemeral ports; provenance keeps the actual bytes in
// the archive only, so repeated runs file identical measurements.
function withoutProvenance(value) {
  if (Array.isArray(value)) return value.map(withoutProvenance);
  if (value === null || typeof value !== "object") return value;
  const out = {};
  for (const [name, item] of Object.entries(value)) {
    if (name !== "provenance") out[name] = withoutProvenance(item);
  }
  return out;
}

function filingMember(result) {
  const { observation, failures, evidence_selector, ...shared } = result.measurements ?? {};
  return {
    ...result,
    measurements: {
      ...compactMeasurements(shared),
      observation: withoutProvenance(observation),
      ...(evidence_selector ? { evidence_selector } : {}),
    },
  };
}

function filingAggregate(aggregate) {
  return {
    rule: aggregate.rule,
    target: aggregate.target,
    status: aggregate.status,
    severity: aggregate.severity,
    measurements: compactMeasurements(aggregate.measurements),
    evidence: aggregate.evidence ?? [],
    fingerprint: aggregate.fingerprint,
  };
}

function statusCounts(results) {
  const counts = { pass: 0, fail: 0, error: 0, skipped: 0 };
  for (const result of results) if (Object.hasOwn(counts, result.status)) counts[result.status] += 1;
  return counts;
}

// `results` are runner results after member expansion; `aggregates` are the
// expanded aggregate results, keyed by their fingerprint.
export function filingDocument({ surface, results, aggregates = [], ownerContext = null }) {
  const tracked = trackedGroups(ownerContext, surface);
  const failing = new Set(results.filter((result) => result.defect && result.status === "fail")
    .map((result) => groupKeyOf(surface, result)));
  const incomplete = new Set(results.filter((result) => result.original_fingerprint && result.status !== "pass")
    .map((result) => result.original_fingerprint));
  const reported = [];
  let omitted = 0;
  const observed = new Set();
  for (const result of results) {
    if (!result.original_fingerprint) {
      reported.push(result);
      continue;
    }
    const key = result.defect ? groupKeyOf(surface, result) : "";
    if (key) observed.add(key);
    if (result.status !== "pass" || !key || failing.has(key) || tracked.has(key)) reported.push(filingMember(result));
    else omitted += 1;
  }
  // A failing aggregate is represented by its members. Any non-pass member
  // withholds the aggregate pass, so incomplete capture never earns credit.
  let reportedAggregates = 0;
  for (const aggregate of aggregates) {
    if (aggregate.status === "fail") continue;
    if (aggregate.status === "pass" && incomplete.has(aggregate.fingerprint)) continue;
    reported.push(filingAggregate(aggregate));
    reportedAggregates += 1;
  }
  reported.sort((left, right) => left.fingerprint.localeCompare(right.fingerprint));
  const groups = new Set(results.filter((result) => result.defect).map((result) => groupKeyOf(surface, result)));
  return {
    results: reported,
    summary: {
      ...statusCounts(results),
      observations: results.length,
      defect_groups: groups.size,
      reported: reported.length,
      reported_aggregates: reportedAggregates,
      omitted_untracked_passing: omitted,
      tracked_groups: tracked.size,
      tracked_groups_unobserved: [...tracked].filter((key) => !observed.has(key)).length,
    },
  };
}

// Write `raw` as compact JSON, gzip it once, and split the gzip stream into
// parts. Concatenating the parts in order and inflating them reproduces the
// exact JSON bytes the descriptor's uncompressed digest names.
export async function writeObservationArchive(outDir, raw) {
  const json = Buffer.from(`${JSON.stringify(raw)}\n`);
  const gzip = gzipSync(json, { level: 9 });
  const directory = path.join(outDir, "archive");
  await mkdir(directory, { recursive: true });
  const parts = [];
  for (let offset = 0, index = 0; offset < gzip.length; offset += ARCHIVE_PART_BYTES, index += 1) {
    const bytes = gzip.subarray(offset, offset + ARCHIVE_PART_BYTES);
    const relative = path.posix.join("archive", `observations.part${String(index).padStart(2, "0")}.gz`);
    await writeFile(path.join(outDir, relative), bytes);
    parts.push({ path: relative, bytes: bytes.length, sha256: sha256(bytes) });
  }
  return {
    schema_version: 1,
    encoding: "gzip",
    media_type: "application/json",
    observation_count: raw.results.length,
    aggregate_count: raw.aggregates?.length ?? 0,
    uncompressed_bytes: json.length,
    uncompressed_sha256: sha256(json),
    gzip_bytes: gzip.length,
    gzip_sha256: sha256(gzip),
    parts,
  };
}
