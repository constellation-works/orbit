// Producer policy stays here; the filer consumes only opaque defect keys.
import { createHash } from "node:crypto";

const declared = new WeakMap();
const digest = (value) => createHash("sha256").update(JSON.stringify(value)).digest("hex");

// The declared observations before member deduplication, for the raw archive.
export function declaredUiObservations(result) {
  return declared.get(result) ?? [];
}

export function declareUiObservations(result, observations) {
  if (observations.length) declared.set(result, observations);
  return result;
}

export function elementObservations(result, rows, failures, discriminators = ["constraint"]) {
  const observations = [];
  for (const row of rows) {
    if (!row?.element?.key) continue;
    for (const discriminator of row.discriminators ?? discriminators) {
      const identity = [row.element.scope === "shared" ? "shared-component" : result.target.view,
        result.target.theme, result.target.state, row.element.key, discriminator];
      const matched = failures.filter((item) => item.element?.key === row.element.key &&
        (item.discriminator ?? "constraint") === discriminator);
      // Every repeated failure is declared, so the raw archive keeps it; the
      // filing member keeps the first one.
      if (!matched.length) observations.push({ status: "pass", identity, measurement: row });
      for (const failure of matched) observations.push({ status: "fail", identity, measurement: failure });
    }
  }
  // One ambiguous row must not erase proven identities on the same page.
  // Unknown observations carry no shared defect key. Their local
  // selector/discriminator and duplicate occurrence distinguish them without guessing a
  // cross-target cause or granting future aggregate-pass credit.
  const occurrences = new Map();
  const unidentified = [];
  for (const failure of failures.filter((row) => !row?.element?.key)) {
    if (!failure?.selector && !failure?.tag) { unidentified.push(failure); continue; }
    const localIdentity = [failure.discriminator ?? failure.property ?? "constraint", failure.selector ?? "", failure.tag ?? ""];
    const localKey = digest(localIdentity);
    const occurrence = occurrences.get(localKey) ?? 0;
    occurrences.set(localKey, occurrence + 1);
    observations.push({ status: "fail", ungrouped: true,
      identity: ["unknown", result.fingerprint, ...localIdentity, occurrence], measurement: failure });
  }
  if (unidentified.length) observations.push({ status: "fail", ungrouped: true,
    identity: ["unknown", result.fingerprint, "unidentified"], measurement: { unknown_failures: unidentified } });
  return declareUiObservations(result, observations);
}

// Called only by the public sweep runner. check()/verdict() keep their legacy
// aggregate status/fingerprint for visual delivery and other direct clients.
export function expandUiObservations(result) {
  const observations = declared.get(result);
  if (!observations?.length) return [result];
  const unique = new Map();
  // Sibling rows belong to other observations. Repeating the aggregate arrays
  // in every member both misattributes measurements and quadratically expands
  // reports against the unchanged 4 MiB adapter budget.
  const measurements = Object.fromEntries(Object.entries(result.measurements).filter(([name, value]) =>
    name !== "failures" && !(Array.isArray(value) && value.some((row) => row?.element || row?.selector))));
  for (const observation of observations) {
    const key = digest(observation.identity);
    const member = {
      ...result,
      status: observation.status,
      original_fingerprint: result.fingerprint,
      fingerprint: `${result.fingerprint}|member|${key}`,
      ...(observation.ungrouped ? {} : { defect: { version: 1, key } }),
      measurements: {
        ...measurements,
        failures: observation.status === "fail" ? [observation.measurement] : [],
        observation: observation.measurement,
        ...(observation.measurement?.element?.evidence_selector
          ? { evidence_selector: observation.measurement.element.evidence_selector } : {}),
      },
      evidence: [...result.evidence],
    };
    // Repeated measurements of one known cause do not create duplicate members.
    const previous = unique.get(key);
    if (!previous || (member.status === "fail" && previous.status !== "fail")) unique.set(key, member);
  }
  return [...unique.values()];
}
