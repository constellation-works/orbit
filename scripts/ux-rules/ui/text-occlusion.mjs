// Visible text painted under an opaque element or another text run.
//
// A reader cannot read text that another element paints over, even when the
// DOM order looks fine. The Analytics implementers detail squeezes its label
// column to nothing, so the value ("42 measured ...") paints over its label.
// Sample points inside each text run's own line boxes (a container's box can
// extend past its glyphs). A sample is occluded only when an element painted
// above it actually paints at that point: either its own glyphs' line boxes
// contain the point, or an opaque background (or replaced content) covers
// it. Hit testing alone does not answer that question: a wide transparent
// element is hit over its empty area where it paints nothing, and an opaque
// element with `pointer-events: none` paints while never being hit. The
// paint stack is therefore read with pointer events temporarily forced on,
// and every element above the sample is checked against its painted
// geometry rather than its hit-test box.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.text-occlusion",
  title: "Visible text is not painted over by another element",
  severity: "high",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const colorAlpha = (value) => {
      const match = String(value ?? "").match(/rgba?\(([^)]+)\)/);
      if (!match) return String(value ?? "").trim() === "transparent" ? 0 : 1;
      const alpha = Number.parseFloat(match[1].split(",")[3] ?? "1");
      return Number.isFinite(alpha) ? alpha : 1;
    };
    const isOpaque = (style) => colorAlpha(style.backgroundColor) > 0.05;
    const ownText = (el) =>
      [...el.childNodes].some(
        (node) => node.nodeType === 3 && node.textContent.trim() !== "",
      );
    // Line boxes of the element's own glyphs, so samples land on paint.
    const textBoxes = (el) => {
      try {
        const range = document.createRange();
        range.selectNodeContents(el);
        return [...range.getClientRects()]
          .filter((r) => r.width >= 4 && r.height > 0)
          .map((r) => ({ left: r.left, top: r.top, right: r.right, bottom: r.bottom }));
      } catch {
        return [];
      }
    };
    // Line boxes of every text node under an element, tight to where its
    // glyphs paint. A container's border box (and the line box of a block
    // with, say, right-aligned text) can extend far past those glyphs, so
    // neither proves that the element paints at a given point.
    const glyphRectsCache = new Map();
    const glyphRects = (el) => {
      if (glyphRectsCache.has(el)) return glyphRectsCache.get(el);
      const rects = [];
      try {
        const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
        let node = walker.nextNode();
        while (node) {
          if (node.textContent.trim() !== "") {
            const parent = node.parentElement;
            const style = parent ? getComputedStyle(parent) : null;
            const invisible =
              !style ||
              style.display === "none" ||
              style.visibility === "hidden" ||
              Number.parseFloat(style.opacity) === 0 ||
              colorAlpha(style.color) <= 0.05 ||
              H.px(style.fontSize) <= 0;
            if (!invisible) {
              const range = document.createRange();
              range.selectNode(node);
              for (const r of range.getClientRects()) {
                if (r.width > 0 && r.height > 0) {
                  rects.push({ left: r.left, top: r.top, right: r.right, bottom: r.bottom });
                }
              }
            }
          }
          node = walker.nextNode();
        }
      } catch {
        // No measurable glyphs: the element paints no text.
      }
      glyphRectsCache.set(el, rects);
      return rects;
    };
    const containsPoint = (rect, x, y) =>
      x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    // Whether an element painted above a sample actually paints at it:
    // its glyphs are there, or an opaque fill / replaced content covers it.
    const paintsAt = (el, x, y) => {
      const style = getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") return false;
      if (Number.parseFloat(style.opacity) === 0) return false;
      if (glyphRects(el).some((rect) => containsPoint(rect, x, y))) return true;
      const box = el.getBoundingClientRect();
      if (!containsPoint(box, x, y)) return false;
      if (isOpaque(style)) return true;
      return ["IMG", "VIDEO", "CANVAS", "IFRAME"].includes(el.tagName);
    };
    const candidates = [];
    for (const el of H.all("*")) {
      if (el.tagName === "svg" || el.closest("svg")) continue;
      if (el.closest("details:not([open]), [hidden]")) continue;
      if (!ownText(el)) continue;
      if (H.px(getComputedStyle(el).fontSize) <= 0) continue;
      candidates.push(el);
      if (candidates.length >= 150) break;
    }
    const failures = [];
    let samples = 0;
    // Paint order is stacking order, but elementsFromPoint only returns
    // hit-testable elements: an opaque `pointer-events: none` overlay
    // paints over text while never appearing in the stack. Force pointer
    // events on for the stack reads (paint and layout are unaffected),
    // then restore the page before returning.
    const hitStyle = document.createElement("style");
    hitStyle.textContent = "*,*::before,*::after{pointer-events:auto !important}";
    (document.head ?? document.documentElement).appendChild(hitStyle);
    try {
      for (const el of candidates) {
        const boxes = textBoxes(el).slice(0, 3);
        const points = [];
        for (const box of boxes) {
          const y = (box.top + box.bottom) / 2;
          for (const x of [box.left + 2, (box.left + box.right) / 2, box.right - 2]) {
            if (x > box.left && x < box.right) points.push({ x, y });
            if (points.length >= 5) break;
          }
          if (points.length >= 5) break;
        }
        for (const { x, y } of points) {
          if (samples >= 400) break;
          samples += 1;
          if (x < 0 || y < 0 || x > innerWidth || y > innerHeight) continue;
          const stack =
            typeof document.elementsFromPoint === "function"
              ? document.elementsFromPoint(x, y)
              : [document.elementFromPoint(x, y)].filter(Boolean);
          const at = stack.indexOf(el);
          if (at === -1) continue;
          let covered = null;
          for (const above of stack.slice(0, at)) {
            if (above === el || el.contains(above) || above.contains(el)) continue;
            if (!paintsAt(above, x, y)) continue;
            covered = above;
            break;
          }
          if (covered) {
            failures.push({
              selector: H.selectorOf(el),
              covered_by: H.selectorOf(covered),
              point_x_px: Math.round(x),
              point_y_px: Math.round(y),
              text: (el.textContent ?? "").trim().slice(0, 80),
            });
            break;
          }
        }
        if (failures.length >= 20 || samples >= 400) break;
      }
    } finally {
      hitStyle.remove();
    }
    return { matched: candidates.length, failures };
  });
  return verdict(ctx, rule, found.matched, found.failures);
}
