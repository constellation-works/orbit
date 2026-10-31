// Document and active pane never overflow horizontally.
import { measureInPage, uiResultOf } from "../lib/ui-context.mjs";
export const rule = { id: "ui.no-horizontal-overflow", title: "No page or active-pane horizontal overflow", severity: "high", surface: "ui", targets: { views: "*", widths: [1440, 1166, 375] } };
export async function check(ctx) {
  const m = await measureInPage(ctx, (H) => {
    const d = document.documentElement;
    const pane = H.activeRoot();
    const bounded = (el) => { const s = getComputedStyle(el); return s.overflowX === "auto" || s.overflowX === "scroll" || s.overflowX === "hidden"; };
    const offenders = [];
    for (const el of H.all("*", pane)) {
      if (offenders.length >= 10) break;
      if (bounded(el)) continue;
      const r = H.rect(el);
      if (r.right > d.clientWidth + 1 && r.left >= -1) offenders.push({ selector: H.selectorOf(el), right: Math.round(r.right), client_width: d.clientWidth });
    }
    return { doc_scroll: d.scrollWidth, doc_client: d.clientWidth, pane_scroll: pane.scrollWidth ?? 0, pane_client: pane.clientWidth ?? 0, offenders };
  });
  const excess = Math.max(m.doc_scroll - m.doc_client, m.pane_scroll - m.pane_client);
  const failed = excess > 1;
  return uiResultOf(rule, ctx.target, failed ? "fail" : "pass", {
    selector: failed ? (m.offenders[0]?.selector ?? "viewport") : "",
    measurements: { ...m, excess_px: Math.max(0, excess), region: "viewport" },
  });
}
