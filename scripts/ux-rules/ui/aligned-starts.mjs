// Neighbouring columns' first content starts at the same top.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.aligned-starts", title: "Neighbouring columns start at the same top", severity: "low", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const rows = await measureInPage(ctx, (H) => {
    const result = [];
    for (const parent of H.all("*")) {
      const s = getComputedStyle(parent);
      const isRow = s.display === "grid" || (s.display === "flex" && s.flexDirection.startsWith("row"));
      if (!isRow) continue;
      const children = [...parent.children].filter((child) => H.visible(child));
      if (children.length < 2) continue;
      const tops = children.map((child) => Math.round(H.rect(child).top));
      if (Math.max(...tops) - Math.min(...tops) > 4) continue;
      const starts = children.map((child) => {
        const first = H.visible(child.firstElementChild) ? child.firstElementChild : child;
        return { selector: H.selectorOf(child), first_top_px: Math.round(H.rect(first).top), top_px: Math.round(H.rect(child).top) };
      });
      result.push({ parent: H.selectorOf(parent), starts });
      if (result.length >= 30) break;
    }
    return result;
  });
  let matched = 0; const failures = [];
  for (const row of rows) {
    matched += row.starts.length;
    const expected = Math.min(...row.starts.map((s) => s.first_top_px));
    for (const start of row.starts) if (start.first_top_px - expected > 8) failures.push({ ...start, parent: row.parent, sibling_first_top_px: expected, delta_px: start.first_top_px - expected });
  }
  return verdict(ctx, rule, matched, failures);
}
