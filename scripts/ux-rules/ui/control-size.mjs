// Buttons in one toolbar or row share height, padding, font size and weight.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.control-size", title: "Buttons in one row share size and type", severity: "low", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const groups = await measureInPage(ctx, (H) => {
    const result = [];
    for (const parent of H.all("*")) {
      const buttons = [...parent.querySelectorAll(":scope > button, :scope > a.action, :scope > .action")].filter((el) => H.visible(el));
      if (buttons.length < 2) continue;
      result.push({ parent: H.selectorOf(parent), items: buttons.map((el) => { const s = getComputedStyle(el); const r = H.rect(el); return { selector: H.selectorOf(el), height_px: Math.round(r.height), top: Math.round(r.top), padding: s.padding, font_size: s.fontSize, font_weight: s.fontWeight }; }) });
      if (result.length >= 40) break;
    }
    return result;
  });
  let matched = 0; const failures = [];
  for (const group of groups) {
    const rows = new Map();
    for (const item of group.items) { const key = String(item.top); if (!rows.has(key)) rows.set(key, []); rows.get(key).push(item); }
    for (const items of rows.values()) {
      if (items.length < 2) continue;
      matched += items.length;
      const modal = (key) => [...items].sort((a, b) => items.filter((i) => i[key] === b[key]).length - items.filter((i) => i[key] === a[key]).length)[0][key];
      for (const item of items) {
        if (Math.abs(item.height_px - modal("height_px")) > 2) failures.push({ selector: item.selector, parent: group.parent, height_px: item.height_px, sibling_height_px: modal("height_px") });
        else if (item.font_size !== modal("font_size")) failures.push({ selector: item.selector, parent: group.parent, font_size: item.font_size, sibling_font_size: modal("font_size") });
      }
    }
  }
  return verdict(ctx, rule, matched, failures);
}
