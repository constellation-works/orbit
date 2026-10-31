// No bordered or filled box directly inside a bordered panel without grouping role.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.nested-card", title: "No card nested directly inside a panel", severity: "low", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
const ALLOW = new Set(["TABLE", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "A", "CODE", "PRE", "svg"]);
export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const bordered = (el) => { const s = getComputedStyle(el); return H.px(s.borderTopWidth) > 0 && s.borderTopStyle !== "none"; };
    const filled = (el) => { const s = getComputedStyle(el); return s.backgroundColor !== "rgba(0, 0, 0, 0)" && s.backgroundColor !== "transparent"; };
    const result = [];
    for (const panel of H.all(".panel")) {
      if (!bordered(panel)) continue;
      for (const child of [...panel.children]) {
        if (!H.visible(child)) continue;
        result.push({ panel: H.selectorOf(panel), selector: H.selectorOf(child), tag: child.tagName, role: child.getAttribute("role") ?? "", bordered: bordered(child), filled: filled(child), labelled: child.hasAttribute("aria-label") || child.hasAttribute("aria-labelledby") });
      }
    }
    return result;
  });
  const failures = found.filter((item) => (item.bordered || item.filled) && !ALLOW.has(item.tag) && !item.role && !item.labelled);
  return verdict(ctx, rule, found.length, failures);
}
