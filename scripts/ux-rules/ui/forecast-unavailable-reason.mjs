// API-shaped fixture rule: an unavailable backlog forecast names its reason in the DOM.
// The API (GET /api/diagnostics/backlog_forecast) is the fixture: when it reports
// unavailable reasons, the rendered view must carry data-forecast-reason with a
// known kind and visible reason text, never a bare "unavailable".
import { measureInPage, uiResultOf } from "../lib/ui-context.mjs";
export const KNOWN_REASONS = ["regression_history_missing", "risk_unestimated", "arrival_conflict_unknown", "insufficient_history", "empty", "transport", "unknown"];
export const rule = { id: "ui.forecast-unavailable-reason", title: "Unavailable backlog forecast names its reason", severity: "medium", surface: "ui", targets: { views: ["tasks-list", "analytics-forecast"], widths: [1440] } };
function apiReasons(payload) {
  const forecast = payload?.forecast ?? payload;
  const unavailable = forecast?.unavailable;
  if (!unavailable || typeof unavailable !== "object") return [];
  return Object.entries(unavailable).filter(([, value]) => Boolean(value)).map(([kind]) => kind);
}
export async function check(ctx) {
  const target = ctx.target;
  let api;
  try {
    // The dashboard scopes every API read with ?workspace=; fetch in the
    // page, in its selected scope, falling back to the aggregate token.
    api = await ctx.page.evaluate(async () => {
      const scope = new URLSearchParams(location.search).get("workspace") ?? "all";
      const response = await fetch(`/api/diagnostics/backlog_forecast?workspace=${encodeURIComponent(scope)}`);
      const text = await response.text();
      let json = null;
      try { json = JSON.parse(text); } catch { json = null; }
      return { status: response.status, json };
    });
  }
  catch (error) { return uiResultOf(rule, target, "error", { measurements: { message: String(error?.message ?? error) } }); }
  if (api.status !== 200 || api.json === null) {
    return uiResultOf(rule, target, "skipped", { measurements: { reason: "forecast API not reachable on this target", status: api.status } });
  }
  const reasons = apiReasons(api.json);
  const dom = await measureInPage(ctx, (H) => H.all("[data-forecast-reason]").map((el) => ({ reason: el.dataset.forecastReason, text: (el.textContent ?? "").trim().slice(0, 160), selector: H.selectorOf(el) })));
  if (reasons.length === 0) {
    return uiResultOf(rule, target, "pass", { measurements: { api_unavailable: false, dom_reasons: dom } });
  }
  const named = dom.filter((entry) => KNOWN_REASONS.includes(entry.reason) && entry.text !== "");
  const covers = named.some((entry) => reasons.includes(entry.reason)) || (named.length > 0 && reasons.every((r) => KNOWN_REASONS.includes(r)));
  return uiResultOf(rule, target, covers ? "pass" : "fail", {
    selector: covers ? "" : "[data-forecast-reason]",
    measurements: { api_unavailable: true, api_reasons: reasons, dom_reasons: dom, known_reasons: KNOWN_REASONS },
  });
}
