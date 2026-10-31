// Content wider than its container where clipping cuts it or it hits a sibling.
//
// Bounded scroll wrappers (`overflow-x: auto/scroll`, or content inside one)
// are exempt: sideways scrolling is their contract, matching
// `ui.no-horizontal-overflow`. An ellipsis (`text-overflow: ellipsis`) is an
// intentional cut with its own indicator, not a defect. Everything else whose
// `scrollWidth` exceeds its `clientWidth` is flagged only when a clipping
// ancestor (`hidden`/`clip`) cuts it or its overflow box runs into a visible
// sibling.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.text-overflow",
  title: "Content wider than its container is not cut or run into a sibling",
  severity: "medium",
  surface: "ui",
  targets: { views: "*", widths: [1440, 1166, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const scrollableX = (style) => style.overflowX === "auto" || style.overflowX === "scroll";
    const clippedX = (style) => style.overflowX === "hidden" || style.overflowX === "clip";
    const inScrollable = (el) => {
      let node = el.parentElement;
      while (node && node !== document.body) {
        if (scrollableX(getComputedStyle(node))) return true;
        node = node.parentElement;
      }
      return false;
    };
    const clippedByAncestor = (el) => {
      let node = el;
      while (node && node !== document.body) {
        if (clippedX(getComputedStyle(node))) return true;
        node = node.parentElement;
      }
      return false;
    };
    const SKIP_TAGS = new Set(["HTML", "BODY", "INPUT", "TEXTAREA", "SELECT", "svg"]);
    const candidates = [];
    for (const el of H.all("*")) {
      if (SKIP_TAGS.has(el.tagName) || el.closest("svg")) continue;
      if (el.closest("details:not([open]), [hidden]")) continue;
      const box = H.rect(el);
      // Screen-reader-only text (1px clipped boxes) and collapsed boxes are
      // not visible overflow.
      if (box.width < 2 || box.height < 2 || el.clientWidth < 2) continue;
      if (scrollableX(getComputedStyle(el))) continue;
      if (el.scrollWidth <= el.clientWidth + 1) continue;
      if (!(el.textContent ?? "").trim()) continue;
      candidates.push(el);
      if (candidates.length >= 400) break;
    }
    const failures = [];
    for (const el of candidates) {
      if (inScrollable(el)) continue;
      const style = getComputedStyle(el);
      if (style.textOverflow === "ellipsis" && style.whiteSpace === "nowrap") continue;
      if (style.whiteSpace === "nowrap" && style.overflowX === "visible") {
        // A nowrap run with no clipping still counts only when it reaches a
        // sibling; otherwise it hangs into empty padding.
      }
      const r = H.rect(el);
      const overflowRight = r.left + el.scrollWidth;
      const clipped = clippedX(style) || clippedByAncestor(el.parentElement);
      let hitsSibling = false;
      let siblingSelector = "";
      const parent = el.parentElement;
      if (parent) {
        for (const sibling of [...parent.children]) {
          if (sibling === el || !H.visible(sibling)) continue;
          const s = H.rect(sibling);
          const vertical = Math.min(r.bottom, s.bottom) - Math.max(r.top, s.top);
          if (vertical > 2 && s.left < overflowRight - 1 && s.left >= r.left) {
            hitsSibling = true;
            siblingSelector = H.selectorOf(sibling);
            break;
          }
        }
      }
      if (!clipped && !hitsSibling) continue;
      failures.push({
        selector: H.selectorOf(el),
        tag: el.tagName.toLowerCase(),
        scroll_width_px: el.scrollWidth,
        client_width_px: el.clientWidth,
        excess_px: el.scrollWidth - el.clientWidth,
        clipped,
        sibling: siblingSelector,
        text: (el.textContent ?? "").trim().slice(0, 80),
      });
      if (failures.length >= 20) break;
    }
    return { matched: candidates.length, failures };
  });
  return verdict(ctx, rule, found.matched, found.failures);
}
