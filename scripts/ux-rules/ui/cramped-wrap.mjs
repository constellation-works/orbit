// A short label wrapping to one word per line.
//
// When a label column is squeezed narrower than its longest word allows, a
// two-to-five-word label stacks vertically ("Cost" / "coverage" / "...").
// Count rendered line boxes with a Range over the element's text: a short
// label whose line count reaches its word count (or averages ~1 word per
// line across three or more lines) is cramped.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const MAX_WORDS = 5;

export const rule = {
  id: "ui.cramped-wrap",
  title: "Short labels do not wrap to one word per line",
  severity: "medium",
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
        // Adjacent rects on one visual line share a top within half a line.
        const lines = [];
        for (const rect of rects) {
          const line = lines.find((each) => Math.abs(each - rect.top) < rect.height / 2);
          if (line === undefined) lines.push(rect.top);
        }
        return lines.length || 1;
      } catch {
        return 1;
      }
    };
    const seen = new Set();
    const rows = [];
    for (const el of H.all("*")) {
      if (el.tagName === "svg" || el.closest("svg")) continue;
      const own = [...el.childNodes]
        .filter((node) => node.nodeType === 3 && node.textContent.trim() !== "")
        .map((node) => node.textContent.trim())
        .join(" ");
      if (!own) continue;
      // Only the innermost text holder counts; a wrapper's lines are its
      // child's lines.
      const nested = [...el.querySelectorAll("*")].some((child) =>
        [...child.childNodes].some(
          (node) => node.nodeType === 3 && node.textContent.trim() !== "",
        ),
      );
      if (nested) continue;
      const words = own.split(/\s+/).filter(Boolean);
      if (words.length < 2 || words.length > 5) continue;
      if (own.length > 60) continue;
      const key = `${H.selectorOf(el)}|${own}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const lines = lineCount(el);
      rows.push({
        selector: H.selectorOf(el),
        words: words.length,
        lines,
        words_per_line: Math.round((words.length / Math.max(1, lines)) * 100) / 100,
        width_px: Math.round(H.rect(el).width),
        text: own.slice(0, 80),
      });
      if (rows.length >= 300) break;
    }
    // A single wrap to two lines is normal responsive behavior; three or
    // more lines at about one word per line means the column is narrower
    // than the label's longest words allow.
    const failures = rows.filter(
      (row) => row.lines >= 3 && row.words / row.lines <= 1.5,
    );
    return { matched: rows.length, failures: failures.slice(0, 20) };
  });
  return verdict(ctx, rule, found.matched, found.failures);
}
