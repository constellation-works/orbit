// Bounded actual browser diagnostics. Keep endpoint query and source context:
// neither a status number nor a message alone establishes a shared cause.
import { declareUiObservations } from "./ui-observations.mjs";

export const DIAGNOSTIC_LIMIT = 100;
export function recordDiagnostic(list, entry) {
  if (list.length < DIAGNOSTIC_LIMIT) list.push(entry);
  else list.dropped = (list.dropped ?? 0) + 1;
}

export function normalizedEndpoint(value, baseUrl) {
  try {
    const url = new URL(value, baseUrl);
    const base = new URL(baseUrl);
    url.hash = "";
    // Fixture ports change on each run. Other origins retain their identity.
    return url.origin === base.origin ? `${url.pathname}${url.search}` : url.href;
  } catch { return String(value ?? "").slice(0, 1000); }
}

function normalizedSource(value, baseUrl) {
  return String(value ?? "").replaceAll(baseUrl.replace(/\/$/, ""), "<app>").slice(0, 2000);
}

export function consoleObservations(result, diagnostics, target, baseUrl) {
  const observations = [];
  // Omitted successful responses only withhold credit from the causes they
  // would have repaired; they never make the failure capture incomplete.
  const truncated = Object.fromEntries(Object.entries(diagnostics).filter(([name, value]) =>
    name !== "successfulResponses" && value?.dropped > 0).map(([name, value]) => [name, value.dropped]));
  const incomplete = Object.keys(truncated).length > 0;
  const network = [];
  const contexts = diagnostics.requestContexts ?? [];
  const networkEntry = (entry, kind, status) => {
    const method = String(entry.method ?? "GET").toUpperCase();
    const endpoint = normalizedEndpoint(entry.url, baseUrl);
    const sources = [...new Set(contexts.filter((context) =>
      context.method === method && normalizedEndpoint(context.url, baseUrl) === endpoint)
      .map((context) => normalizedSource(context.source, baseUrl)).filter(Boolean))].sort();
    // Only an observed single initiator permits sharing across pages. A
    // browser-generated resource error without a JS initiator remains local.
    const stack = entry.source ? normalizedSource(entry.source, baseUrl) : sources.length === 1 ? sources[0] : "";
    const source = stack;
    const identity = [source ? "shared-request-source" : target.view,
      target.theme, target.state, method, endpoint, kind, source,
      ...(!source && sources.length > 1 ? [target.id, sources] : [])];
    // Fixture origins carry an ephemeral port: identities and filed
    // measurements use the normalized forms, and the actual URLs stay in
    // `provenance`, which only the raw archive keeps.
    const { url, source_location: location, ...rest } = entry;
    const measurement = { kind, ...rest, method, endpoint, source, stack,
      source_location: location ? { ...location, url: normalizedSource(location.url, baseUrl) } : null,
      ...(sources.length > 1 ? { ambiguous_sources: sources } : {}),
      provenance: { url, source_location: location ?? null } };
    const observation = { status: incomplete && status === "pass" ? "error" : status, identity, measurement };
    observations.push(observation);
    network.push(observation);
  };
  for (const entry of diagnostics.responses ?? []) {
    networkEntry(entry, "http-response", entry.status >= 400 ? "fail" : "pass");
  }
  for (const entry of diagnostics.successfulResponses ?? []) networkEntry(entry, "http-response", "pass");
  for (const entry of diagnostics.failedRequests ?? []) networkEntry(entry, "request-failed", "fail");
  for (const entry of diagnostics.blockedNonGetOnLoad ?? []) networkEntry(entry, "non-get-blocked", "fail");
  for (const value of diagnostics.offLoopback ?? []) {
    networkEntry(typeof value === "string" ? { url: value } : value, "off-loopback", "fail");
  }
  for (const [kind, entries] of [["page-error", diagnostics.pageErrors ?? []], ["console-error", diagnostics.consoleErrors ?? []]]) {
    for (const value of entries) {
      const actual = typeof value === "string" ? { message: value } : value;
      const entry = { ...actual, message: normalizedSource(actual.message, baseUrl) };
      const location = entry.location?.url ? {
        ...entry.location, url: normalizedEndpoint(entry.location.url, baseUrl),
      } : null;
      // Chromium's resource console line duplicates its HTTP response; retain
      // the actual response cause rather than filing a second generic message.
      const response = kind === "console-error" && /^Failed to load resource:/.test(entry.message) && location
        ? network.find((item) => item.status === "fail" && item.measurement.endpoint === location.url) : null;
      if (response) {
        response.measurement.console = { ...entry, location };
        continue;
      }
      const source = normalizedSource(entry.stack ?? "", baseUrl);
      const genericStatus = /^(?:HTTP(?:\/[\d.]+)?\s+)?[45]\d\d(?:\s+[A-Za-z ]+)?$/i.test(entry.message.trim());
      const knownSource = genericStatus ? "" : source || (location?.url && (location.lineNumber > 0 || location.columnNumber > 0)
        ? JSON.stringify(location) : "");
      observations.push({
        status: "fail",
        identity: [knownSource ? "shared-script-source" : target.id,
          target.theme, target.state, kind, knownSource, entry.name ?? "", entry.message,
          ...(!knownSource ? [location, source] : [])],
        measurement: { kind, ...entry, ...(entry.stack ? { stack: source } : {}), ...(location ? { location } : {}),
          source: knownSource, cause_known: Boolean(knownSource), provenance: actual },
      });
    }
  }
  if (incomplete) observations.push({
    status: "error", identity: [target.id, "diagnostic-limit"],
    measurement: { kind: "diagnostic-limit", truncated },
  });
  // This explicit scope pass is useful as current evidence, but is never a
  // pass for a disappeared exception's distinct cause identity.
  if (observations.every((item) => item.status === "pass")) observations.push({
    status: "pass", identity: [target.view, target.theme, target.state, "console-scope"],
    measurement: { kind: "console-scope", errors: 0 },
  });
  return declareUiObservations(result, observations);
}

// Chromium supplies actual initiators without wrapping application fetch/XHR.
// CDP request ids correlate each response to its own source, including two
// callers of the same endpoint, and never expose request or response payloads.
//
// Failures and successes are kept apart. A failed response is a diagnostic;
// a successful one is only the passing observation that can credit a known
// request cause, deduplicated by method, URL and initiator in its own bounded
// list. Request contexts are retired when their request finishes, so static
// resources cannot exhaust the lists before a real failure arrives. A
// failure whose context was not retained (more than DIAGNOSTIC_LIMIT requests
// in flight) is still recorded, without a source, and marks the context list
// dropped: that is a real coverage gap and withholds pass credit.
export async function captureNetworkDiagnostics(context, page, diagnostics) {
  const session = await context.newCDPSession(page);
  const inFlight = new Map();
  const succeeded = new Set();
  diagnostics.successfulResponses = [];
  const contextGap = () => {
    diagnostics.requestContexts.dropped = (diagnostics.requestContexts.dropped ?? 0) + 1;
  };
  session.on("Network.requestWillBeSent", (event) => {
    // A redirect reuses its request id; the new hop replaces the old one.
    inFlight.delete(event.requestId);
    if (inFlight.size >= DIAGNOSTIC_LIMIT) {
      inFlight.set(event.requestId, null);
      return;
    }
    const frames = event.initiator?.stack?.callFrames ?? [];
    const source = frames.slice(0, 4).map((frame) =>
      `${frame.functionName || "<anonymous>"} (${frame.url}:${frame.lineNumber}:${frame.columnNumber})`).join("\n");
    inFlight.set(event.requestId, { method: event.request.method, url: event.request.url, source,
      source_location: frames[0] ? { url: frames[0].url, line: frames[0].lineNumber, column: frames[0].columnNumber } : null });
  });
  session.on("Network.responseReceived", (event) => {
    const status = event.response.status;
    const entry = inFlight.get(event.requestId);
    if (status < 400) {
      // A success without its context cannot be attributed to a cause.
      if (!entry) return;
      const key = JSON.stringify([entry.method, entry.url, entry.source]);
      if (succeeded.has(key)) return;
      succeeded.add(key);
      recordDiagnostic(diagnostics.successfulResponses, { ...entry, status, resource_type: event.type, failure_class: "none" });
      return;
    }
    if (!entry) contextGap();
    recordDiagnostic(diagnostics.responses, { ...(entry ?? { method: "", url: event.response.url, source: "", source_location: null }),
      status, resource_type: event.type, failure_class: "http-error" });
  });
  session.on("Network.loadingFinished", (event) => inFlight.delete(event.requestId));
  session.on("Network.loadingFailed", (event) => {
    const entry = inFlight.get(event.requestId);
    inFlight.delete(event.requestId);
    // The same-origin filter and failure text come from Playwright's
    // requestfailed event; this keeps only the initiator for that row.
    if (entry) recordDiagnostic(diagnostics.requestContexts, entry);
    else if (entry === null) contextGap();
  });
  await session.send("Network.enable");
}
