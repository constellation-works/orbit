// Siblings of one kind in one container share padding, gap, font size and weight.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.sibling-spacing", title: "Siblings of one kind share spacing and type", severity: "low", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const groups = await measureInPage(ctx, (H) => {
    const result = [];
    for (const parent of H.all("*")) {
      const selected = (el) => el.getAttribute("aria-selected") === "true" || el.getAttribute("aria-pressed") === "true" || el.hasAttribute("aria-current") || (typeof el.className === "string" && /(?:^|\\s)(active|on|selected)(?:\\s|$)/.test(el.className));
      const children = [...parent.children].filter((child) => H.visible(child) && !selected(child));
      if (children.length < 3) continue;
      const byKind = new Map();
      for (const child of children) {
        const kind = child.tagName + "." + (typeof child.className === "string" ? child.className.trim().split(/\s+/)[0] ?? "" : "");
        if (!byKind.has(kind)) byKind.set(kind, []);
        byKind.get(kind).push(child);
      }
      for (const [kind, siblings] of byKind) {
        if (siblings.length < 3) continue;
        result.push({ parent: H.selectorOf(parent), kind, items: siblings.slice(0, 12).map((el) => { const s = getComputedStyle(el); return { selector: H.selectorOf(el), padding: s.padding, font_size: s.fontSize, font_weight: s.fontWeight }; }) });
        if (result.length >= 40) return result;
      }
    }
    return result;
  });
  let matched = 0; const failures = [];
  for (const group of groups) {
    matched += group.items.length;
    const modal = (key) => [...group.items].sort((a, b) => group.items.filter((i) => i[key] === b[key]).length - group.items.filter((i) => i[key] === a[key]).length)[0][key];
    for (const key of ["padding", "font_size", "font_weight"]) {
      const expected = modal(key);
      for (const item of group.items) if (item[key] !== expected) failures.push({ selector: item.selector, parent: group.parent, kind: group.kind, property: key, value: item[key], sibling_value: expected });
    }
  }
  return verdict(ctx, rule, matched, failures);
}
