// Multi-line text with line-height under 1.3x its font size.
//
// The dashboard body sets 1.45-1.65 and prose 1.5; under 1.3 descenders meet
// ascenders. A computed `normal` line-height is unmeasurable from styles
// alone, so it is recorded but never failed: only a numeric ratio under 1.3
// on text that actually renders two or more line boxes fails.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const MIN_LEADING = 1.3;

export const rule = {
  id: "ui.tight-leading",
  title: "Multi-line text has line-height of at least 1.3x font size",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const lineCount = (el) => {
      try {
        const range = document.createRange();
        range.selectNodeContents(el);
        const rects = [...range.getClientRects()].filter((r) => r.width > 0 && r.height > 0);
        const lines = [];
        for (const rect of rects) {
          if (!lines.some((top) => Math.abs(top - rect.top) < rect.height / 2)) {
            lines.push(rect.top);
          }
        }
        return lines.length || 1;
      } catch {
        return 1;
      }
    };
    const rows = [];
    let normalMultiline = 0;
    for (const el of H.all("*")) {
      const own = [...el.childNodes].some(
        (node) => node.nodeType === 3 && node.textContent.trim() !== "",
      );
      if (!own) continue;
      if (el.tagName === "svg" || el.closest("svg")) continue;
      const lines = lineCount(el);
      if (lines < 2) continue;
      const style = getComputedStyle(el);
      const fontSize = H.px(style.fontSize);
      if (!(fontSize > 0)) continue;
      if (style.lineHeight === "normal") {
        normalMultiline += 1;
        continue;
      }
      const lineHeight = H.px(style.lineHeight);
      if (!(lineHeight > 0)) continue;
      rows.push({
        selector: H.selectorOf(el),
        lines,
        line_height_px: Math.round(lineHeight * 10) / 10,
        font_size_px: Math.round(fontSize * 10) / 10,
        ratio: Math.round((lineHeight / fontSize) * 100) / 100,
        text: (el.textContent ?? "").trim().slice(0, 80),
      });
      if (rows.length >= 300) break;
    }
    return {
      matched: rows.length,
      normal_multiline: normalMultiline,
      failures: rows.filter((row) => row.ratio < 1.3).slice(0, 20),
    };
  });
  return verdict(ctx, rule, found.matched, found.failures, {
    min_leading: MIN_LEADING,
    normal_multiline: found.normal_multiline,
  });
}
