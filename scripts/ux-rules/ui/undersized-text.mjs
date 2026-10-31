// Content-bearing UI text under the dashboard minimum.
//
// The theme sets a 14px base (`docs/design/user-interface/specs/theme.md`,
// `base.css`) and its smallest named content size is 11px column heads
// (meta and rail counts sit at 12px). Only content tags count (`p`, `li`,
// `td`, `th`, `dd`, `dt`, `button`, links, labels, headings): micro spans,
// pills and timestamps are not content. Overlines (column
// heads, field labels) are the theme's intentional small text, not a defect,
// so they are exempt, but only the full treatment: uppercase AND tracked
// (letter-spacing >= 5% of the font size). Either style alone is ordinary
// prose. Anything else under 11px fails.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const MIN_FONT_PX = 11;

const CONTENT_TAGS = new Set([
  "P",
  "LI",
  "TD",
  "TH",
  "DD",
  "DT",
  "BUTTON",
  "A",
  "LABEL",
  "H1",
  "H2",
  "H3",
  "H4",
  "H5",
  "H6",
  "SELECT",
  "INPUT",
  "TEXTAREA",
  "OPTION",
  "SUMMARY",
]);

export const rule = {
  id: "ui.undersized-text",
  title: "Content text meets the 11px dashboard minimum",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    // Module scope is not serialized into the page: the set lives here.
    const tags = new Set([
      "P", "LI", "TD", "TH", "DD", "DT", "BUTTON", "A", "LABEL",
      "H1", "H2", "H3", "H4", "H5", "H6", "SELECT", "INPUT",
      "TEXTAREA", "OPTION", "SUMMARY",
    ]);
    const rows = [];
    for (const el of H.all("*")) {
      if (!tags.has(el.tagName)) continue;
      if (el.closest("svg")) continue;
      const text = (el.textContent ?? "").trim();
      // Only the text holder itself, and only real words, not ticks.
      const nested = [...el.querySelectorAll("*")].some((child) =>
        [...child.childNodes].some(
          (node) => node.nodeType === 3 && node.textContent.trim() !== "",
        ),
      );
      if (nested) continue;
      if (text.replace(/[^A-Za-z0-9]/g, "").length < 4) continue;
      const style = getComputedStyle(el);
      const size = H.px(style.fontSize);
      if (!(size > 0)) continue;
      // The overline treatment is both together: uppercase alone or
      // tracking alone is still ordinary prose.
      const overline =
        style.textTransform === "uppercase" &&
        H.px(style.letterSpacing) >= size * 0.05;
      rows.push({
        selector: H.selectorOf(el),
        font_size_px: Math.round(size * 10) / 10,
        overline,
        text: text.slice(0, 80),
      });
      if (rows.length >= 400) break;
    }
    return {
      matched: rows.filter((row) => !row.overline).length,
      // Floor is the theme's smallest named content size: 11px column
      // heads. Smaller text (10.5px copy buttons, micro notes) is a finding.
      failures: rows
        .filter((row) => !row.overline && row.font_size_px < 11)
        .slice(0, 20),
    };
  });
  return verdict(ctx, rule, found.matched, found.failures, {
    min_font_px: MIN_FONT_PX,
  });
}
