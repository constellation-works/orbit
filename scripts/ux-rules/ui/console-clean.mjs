// No page errors, console errors, failed same-origin requests, off-loopback or mutating requests.
import { consoleObservations } from "../lib/ui-console.mjs";
import { uiResultOf } from "../lib/ui-context.mjs";
export const rule = { id: "ui.console-clean", title: "Rendering produces no console, page or request errors", severity: "medium", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const d = ctx.diagnostics ?? { consoleErrors: [], pageErrors: [], failedRequests: [], offLoopback: [], blockedNonGetOnLoad: [] };
  const failures = [
    ...d.pageErrors.map((entry) => ({ kind: "page-error", ...(typeof entry === "string" ? { message: entry } : entry) })),
    ...d.consoleErrors.map((entry) => ({ kind: "console-error", ...(typeof entry === "string" ? { message: entry } : entry) })),
    ...(d.responses ?? []).filter((entry) => entry.status >= 400).map((entry) => ({ kind: "http-response", ...entry })),
    ...d.failedRequests.map((entry) => ({ kind: "request-failed", ...entry })),
    ...d.offLoopback.map((entry) => ({ kind: "off-loopback", ...(typeof entry === "string" ? { url: entry } : entry) })),
    ...d.blockedNonGetOnLoad.map((entry) => ({ kind: "non-get-blocked", ...entry })),
  ];
  const result = uiResultOf(rule, ctx.target, failures.length > 0 ? "fail" : "pass", {
    measurements: { failures: failures.slice(0, 20), counts: { page_errors: d.pageErrors.length, console_errors: d.consoleErrors.length, failed_requests: d.failedRequests.length, off_loopback: d.offLoopback.length, non_get_blocked: d.blockedNonGetOnLoad.length, successful_responses: d.successfulResponses?.length ?? 0, successful_responses_omitted: d.successfulResponses?.dropped ?? 0 } },
  });
  return consoleObservations(result, d, ctx.target, ctx.baseUrl ?? "http://localhost");
}
