// A short label wrapping to one word per line.
//
// When a label column is squeezed narrower than its longest word allows, a
// two-to-five-word label stacks vertically ("Cost" / "coverage" / "...").
// Count rendered line boxes with a Range over the element's text: a short
// label averaging at most 1.5 words across 3+ lines is cramped. Long text
// is cramped at 9+ lines averaging under 25 characters per line. Exclude
// table cells and compact controls from both checks. A visible text holder
// collapsed to zero width is also cramped, even if it occupies only two lines.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const MAX_WORDS = 5;

export const rule = {
  id: "ui.cramped-wrap",
  title: "Text is not squeezed into narrow columns",
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
    // A grid track can collapse to zero while its text still paints outside
    // the box. The shared visibility helper excludes zero-width boxes, so
    // include those whose parent remains visible and whose style is visible.
    const paintsText = (el) => {
      const range = document.createRange();
      range.selectNodeContents(el);
      let rects = [...range.getClientRects()].filter((r) => r.width > 0 && r.height > 0)
        .map((r) => ({ left: r.left, right: r.right, top: r.top, bottom: r.bottom }));
      let scrollX = false;
      let scrollY = false;
      for (let node = el; node && rects.length; node = node.parentElement) {
        const style = getComputedStyle(node);
        if (Number(style.opacity) === 0 || style.visibility !== "visible" || style.display === "none") return false;
        const box = node.getBoundingClientRect();
        // Scroll containers reveal their contents when scrolled; they do not
        // make an otherwise measurable label intentionally hidden.
        const clipX = !scrollX && ["hidden", "clip"].includes(style.overflowX);
        const clipY = !scrollY && ["hidden", "clip"].includes(style.overflowY);
        if (clipX || clipY) rects = rects.map((r) => ({
          left: clipX ? Math.max(r.left, box.left) : r.left,
          right: clipX ? Math.min(r.right, box.right) : r.right,
          top: clipY ? Math.max(r.top, box.top) : r.top,
          bottom: clipY ? Math.min(r.bottom, box.bottom) : r.bottom,
        })).filter((r) => r.right > r.left && r.bottom > r.top);
        scrollX ||= ["auto", "scroll"].includes(style.overflowX);
        scrollY ||= ["auto", "scroll"].includes(style.overflowY);
      }
      return rects.length > 0;
    };
    const elements = [...H.activeRoot().querySelectorAll("*")].filter((el) => {
      if (H.visible(el)) return true;
      const rect = el.getBoundingClientRect();
      const style = getComputedStyle(el);
      return rect.width === 0 && rect.height > 0 && style.display !== "none" &&
        style.visibility === "visible" && el.parentElement && H.visible(el.parentElement) && paintsText(el);
    });
    for (const el of elements) {
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
      if (el.closest("table, button, select, option, [role=button], [role=cell], [role=columnheader], [role=rowheader], .pill, .chip, .badge")) continue;
      const short = words.length >= 2 && words.length <= 5 && own.length <= 60;
      const prose = words.length > 5;
      if (!short && !prose) continue;
      const key = `${H.selectorOf(el)}|${own}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const lines = lineCount(el);
      rows.push({
        selector: H.selectorOf(el),
        words: words.length,
        short,
        chars: own.length,
        chars_per_line: Math.round(own.length / Math.max(1, lines) * 100) / 100,
        lines,
        words_per_line: Math.round((words.length / Math.max(1, lines)) * 100) / 100,
        width_px: Math.round(H.rect(el).width),
        collapsed: H.rect(el).width === 0,
        text: own.slice(0, 80),
      });
    }
    // A single wrap to two lines is normal responsive behavior; three or
    // more lines at about one word per line means the column is narrower
    // than the label's longest words allow.
    const failures = rows.filter(
      (row) => row.collapsed || (row.short && row.lines >= 3 && row.words / row.lines <= 1.5) ||
        (!row.short && row.lines >= 9 && row.chars / row.lines < 25),
    );
    return { matched: rows.length, failures: failures.slice(0, 20) };
  });
  return verdict(ctx, rule, found.matched, found.failures);
}
