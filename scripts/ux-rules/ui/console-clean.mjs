// No page errors, console errors, failed same-origin requests, off-loopback or mutating requests.
import { uiResultOf } from "../lib/ui-context.mjs";
export const rule = { id: "ui.console-clean", title: "Rendering produces no console, page or request errors", severity: "medium", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const d = ctx.diagnostics ?? { consoleErrors: [], pageErrors: [], failedRequests: [], offLoopback: [], blockedNonGetOnLoad: [] };
  const failures = [
    ...d.pageErrors.map((message) => ({ kind: "page-error", message })),
    ...d.consoleErrors.map((message) => ({ kind: "console-error", message })),
    ...d.failedRequests.map((entry) => ({ kind: "request-failed", ...entry })),
    ...d.offLoopback.map((url) => ({ kind: "off-loopback", url })),
    ...d.blockedNonGetOnLoad.map((entry) => ({ kind: "non-get-blocked", ...entry })),
  ];
  return uiResultOf(rule, ctx.target, failures.length > 0 ? "fail" : "pass", {
    measurements: { failures: failures.slice(0, 20), counts: { page_errors: d.pageErrors.length, console_errors: d.consoleErrors.length, failed_requests: d.failedRequests.length, off_loopback: d.offLoopback.length, non_get_blocked: d.blockedNonGetOnLoad.length } },
  });
}
