// Rendered space above a section heading not exceeding the space below it.
//
// A heading introduces the block below it, so the gap above the heading must
// exceed the gap below: otherwise the heading reads as attached to the
// previous block. Rect distances miss collapsed margins (adjoining margins
// draw space while the border boxes touch), so when the border boxes touch
// (within 1px) the larger adjoining margin counts as the rendered space.
// Headings with no following content are skipped: rhythm needs a below.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.heading-rhythm",
  title: "Section headings carry more space above than below",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const rows = [];
    for (const el of H.all("h1, h2, h3, h4, h5, h6")) {
      if (el.closest("svg")) continue;
      if (!(el.textContent ?? "").trim()) continue;
      const parent = el.parentElement;
      if (!parent) continue;
      const siblings = [...parent.children].filter((child) => H.visible(child));
      const at = siblings.indexOf(el);
      if (at === -1 || at === siblings.length - 1) continue;
      const myStyle = getComputedStyle(el);
      const rect = H.rect(el);
      const elTop = rect.top;
      // Rendered space above climbs the ancestor chain: a heading that is
      // the first child of an unpadded wrapper still sits under the gap its
      // section keeps from the previous block.
      let above = null;
      let node = el;
      while (node.parentElement && node !== document.body) {
        const level = node.parentElement;
        const kinds = [...level.children].filter((child) => H.visible(child));
        const pos = kinds.indexOf(node);
        const nodeMargin = H.px(getComputedStyle(node).marginTop);
        let best = -Infinity;
        let bestMargin = 0;
        for (let i = 0; i < pos; i += 1) {
          const r = H.rect(kinds[i]);
          if (r.bottom <= elTop + 1 && r.bottom > best) {
            best = r.bottom;
            bestMargin = H.px(getComputedStyle(kinds[i]).marginBottom);
          }
        }
        if (best > -Infinity) {
          const gap = elTop - best;
          above = gap <= 1 ? gap + Math.max(nodeMargin, bestMargin) : gap;
          break;
        }
        node = level;
      }
      if (above === null) above = elTop;
      const next = H.rect(siblings[at + 1]);
      const nextStyle = getComputedStyle(siblings[at + 1]);
      const gapBelow = next.top - rect.bottom;
      const below =
        gapBelow <= 1
          ? gapBelow + Math.max(H.px(myStyle.marginBottom), H.px(nextStyle.marginTop))
          : gapBelow;
      rows.push({
        selector: H.selectorOf(el),
        above_px: Math.round(above * 10) / 10,
        below_px: Math.round(below * 10) / 10,
        text: (el.textContent ?? "").trim().slice(0, 60),
      });
      if (rows.length >= 200) break;
    }
    return {
      matched: rows.length,
      failures: rows.filter((row) => row.above_px + 1 < row.below_px).slice(0, 20),
    };
  });
  return verdict(ctx, rule, found.matched, found.failures);
}
