// Visible text painted under an opaque element or another text run.
//
// A reader cannot read text that another element paints over, even when the
// DOM order looks fine. The Analytics implementers detail squeezes its label
// column to nothing, so the value ("42 measured ...") paints over its label.
// Sample points inside each text run's own line boxes (a container's box can
// extend past its glyphs) with elementFromPoint: when the hit stack reaches
// the element only after an opaque cover or another text run, the sampled
// text is occluded.
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
    const isOpaque = (style) => {
      const bg = style.backgroundColor ?? "";
      const match = String(bg).match(/rgba?\(([^)]+)\)/);
      if (!match) return false;
      const alpha = Number.parseFloat(match[1].split(",")[3] ?? "1");
      return Number.isFinite(alpha) && alpha > 0.05;
    };
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
          const style = getComputedStyle(above);
          if (!isOpaque(style) && !ownText(above)) continue;
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
    return { matched: candidates.length, failures };
  });
  return verdict(ctx, rule, found.matched, found.failures);
}
