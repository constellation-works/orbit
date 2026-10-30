// Panels side by side in one row have equal heights.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.even-row-panels", title: "Panels in one row have equal heights", severity: "low", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const groups = await measureInPage(ctx, (H) => {
    const byParent = new Map();
    for (const panel of H.all(".panel")) {
      const parent = panel.parentElement;
      if (!parent) continue;
      if (!byParent.has(parent)) byParent.set(parent, []);
      byParent.get(parent).push(panel);
    }
    const result = [];
    for (const [parent, panels] of byParent) {
      if (panels.length < 2) continue;
      const items = panels.map((panel) => { const r = H.rect(panel); return { selector: H.selectorOf(panel), top_px: Math.round(r.top), height_px: Math.round(r.height) }; });
      const rows = new Map();
      for (const item of items) { if (!rows.has(item.top_px)) rows.set(item.top_px, []); rows.get(item.top_px).push(item); }
      for (const rowItems of rows.values()) if (rowItems.length > 1) result.push({ parent: H.selectorOf(parent), items: rowItems });
    }
    return result;
  });
  let matched = 0; const failures = [];
  for (const group of groups) {
    matched += group.items.length;
    const max = Math.max(...group.items.map((i) => i.height_px));
    for (const item of group.items) if (max - item.height_px > 4) failures.push({ ...item, parent: group.parent, sibling_height_px: max, delta_px: max - item.height_px });
  }
  return verdict(ctx, rule, matched, failures);
}
