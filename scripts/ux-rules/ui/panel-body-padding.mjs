// Every panel body keeps a non-zero inset, equal to its sibling panels'.
// The inset that counts is where content actually starts: body padding,
// or the first content element's offset plus its own padding (a summary
// table's first cell, a note's padding). A body whose content touches
// the panel border fails — the Lifecycle diagnostics card regression.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = {
  id: "ui.panel-body-padding",
  title: "Panel bodies have a non-zero inset equal to siblings",
  severity: "medium",
  surface: "ui",
  targets: { views: "*", widths: [1440, 1166, 375] },
};
export async function check(ctx) {
  const rows = await measureInPage(ctx, (H) => {
    const contentInset = (body) => {
      const bodyRect = H.rect(body);
      let element = body;
      let inset = H.px(getComputedStyle(body).paddingLeft);
      for (let depth = 0; depth < 6; depth += 1) {
        const child = [...element.children].find((candidate) => H.visible(candidate));
        if (!child) break;
        element = child;
        inset = Math.max(
          0,
          Math.round(H.rect(element).left - bodyRect.left) +
            H.px(getComputedStyle(element).paddingLeft),
        );
        const ownText = [...element.childNodes].some(
          (node) => node.nodeType === 3 && node.textContent.trim() !== "",
        );
        if (ownText || element.tagName === "TD" || element.tagName === "TH") break;
      }
      return { inset, element };
    };
    const panels = H.all(".panel, .audit-summary-card, .rel-card");
    const measured = [];
    for (const panel of panels) {
      const body =
        panel.querySelector(
          ":scope > .body, :scope > .card-body, :scope > .plugin-panel-body, .card-body, .tile-body",
        ) ?? panel;
      if (!H.visible(body)) continue;
      const style = getComputedStyle(body);
      const { inset, element } = contentInset(body);
      measured.push({
        selector:
          H.selectorOf(body) === H.selectorOf(panel)
            ? H.selectorOf(panel)
            : `${H.selectorOf(panel)} ${H.selectorOf(body)}`,
        panel: H.selectorOf(panel),
        parent: H.selectorOf(panel.parentElement ?? panel),
        content_selector: H.selectorOf(element),
        padding_left_px: H.px(style.paddingLeft),
        padding_top_px: H.px(style.paddingTop),
        effective_inset_px: inset,
      });
    }
    return measured;
  });
  const failures = [];
  for (const row of rows) {
    if (row.effective_inset_px <= 0) {
      failures.push({ ...row, reason: "content touches the panel border (zero inset)" });
    }
  }
  const byParent = new Map();
  for (const row of rows) {
    if (!byParent.has(row.parent)) byParent.set(row.parent, []);
    byParent.get(row.parent).push(row);
  }
  for (const siblings of byParent.values()) {
    if (siblings.length < 2) continue;
    const counts = new Map();
    for (const row of siblings) {
      counts.set(row.effective_inset_px, (counts.get(row.effective_inset_px) ?? 0) + 1);
    }
    const modal = [...counts.entries()].sort((a, b) => b[1] - a[1])[0][0];
    for (const row of siblings) {
      // Sibling bodies legitimately mix table cells (6px) and notes
      // (12px); only a content start far closer than its siblings' is
      // the regression shape.
      if (
        row.effective_inset_px < modal / 2 &&
        modal > 0 &&
        !failures.some((failure) => failure.selector === row.selector)
      ) {
        failures.push({
          ...row,
          sibling_inset_px: modal,
          reason: "inset differs from sibling panels",
        });
      }
    }
  }
  return verdict(ctx, rule, rows.length, failures, { panels: rows.slice(0, 20) });
}
