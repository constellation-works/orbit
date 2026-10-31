// Text closer than a minimum inset to its container's edge.
//
// The dashboard density keeps panels roomy (12-16px headers, 8px gaps), so
// text starting within 4px of a bordered or filled container's border reads
// as touching the edge. Measure each text run's distance to the BORDER box
// of its nearest bordered/filled block-level ancestor (padding counts as
// inset); a run with no such ancestor within three levels has no edge to
// touch and is skipped. Buttons, pills and chips hug by design and are out
// of scope, as are absolutely positioned and screen-reader-only runs.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const MIN_INSET_PX = 4;

export const rule = {
  id: "ui.cramped-padding",
  title: "Text keeps a minimum inset from its container's edge",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const MIN = 4;
    const opaque = (bg) => bg !== "rgba(0, 0, 0, 0)" && bg !== "transparent" && bg !== "";
    const hasEdge = (el) => {
      const s = getComputedStyle(el);
      const bordered = ["Top", "Right", "Bottom", "Left"].some(
        (side) =>
          H.px(s[`border${side}Width`]) > 0 && s[`border${side}Style`] !== "none",
      );
      return bordered || opaque(s.backgroundColor ?? "");
    };
    // Nearest opaque background above the container: a fill counts as an
    // edge only when it differs from what sits behind it.
    const behind = (el) => {
      let node = el.parentElement;
      while (node) {
        const bg = getComputedStyle(node).backgroundColor ?? "";
        if (opaque(bg)) return bg;
        node = node.parentElement;
      }
      return "";
    };
    const ownText = (el) =>
      [...el.childNodes].some(
        (node) => node.nodeType === 3 && node.textContent.trim() !== "",
      );
    const TABLE_PARTS = new Set([
      "TR",
      "TD",
      "TH",
      "THEAD",
      "TBODY",
      "TFOOT",
      "CAPTION",
      "COL",
      "COLGROUP",
    ]);
    const rows = [];
    for (const el of H.all("*")) {
      if (!ownText(el)) continue;
      // Buttons own their insets as control chrome (covered by control-size);
      // table internals lay out by grid, not flow inset; absolutely
      // positioned and screen-reader-only runs are placed by explicit
      // coordinates, not flow inset.
      if (TABLE_PARTS.has(el.tagName) || el.tagName === "BUTTON" || el.tagName === "svg") continue;
      if (el.closest("svg")) continue;
      if (el.closest("details:not([open]), [hidden]")) continue;
      const selfStyle = getComputedStyle(el);
      if (selfStyle.position === "absolute" || selfStyle.position === "fixed") continue;
      const selfBox = H.rect(el);
      if (selfBox.width < 2 || selfBox.height < 2) continue;
      // A run that carries its own edge (a chip, note or band) brings its
      // own padding; the rule measures bare runs against their container.
      if (hasEdge(el)) continue;
      let box = el.parentElement;
      let depth = 0;
      while (box && depth < 3 && !hasEdge(box)) {
        box = box.parentElement;
        depth += 1;
      }
      if (!box || box === document.body || box === document.documentElement) continue;
      if (!hasEdge(box)) continue;
      // Control chrome hugs by design: a button or link container, or a
      // wrapper barely larger than the text itself (pills, chips, tracks),
      // is the text's own badge, not a roomy container.
      if (box.tagName === "BUTTON" || box.tagName === "A") continue;
      const edgeBox = H.rect(box);
      const textArea = Math.max(1, selfBox.width * selfBox.height);
      const boxArea = Math.max(1, edgeBox.width * edgeBox.height);
      if (boxArea < 8 * textArea) continue;
      // Pills, chips and buttons hug their text by design (inline-level
      // boxes); the rule targets block-level containers such as panels,
      // headers and bodies.
      const display = getComputedStyle(box).display;
      if (
        !["block", "flow-root", "flex", "grid", "table", "table-cell", "list-item"].includes(
          display,
        )
      ) {
        continue;
      }
      // Glyph boxes of the element's own text nodes, not the element box:
      // a padded block's glyphs can sit deep inside while its border box
      // touches the container (a flush intro band), line-height struts can
      // hang past real glyphs, and positioned descendants can sit anywhere.
      let glyphs = null;
      try {
        const boxes = [];
        for (const node of el.childNodes) {
          if (node.nodeType !== 3 || node.textContent.trim() === "") continue;
          const range = document.createRange();
          range.selectNode(node);
          for (const r of range.getClientRects()) {
            if (r.width > 0 && r.height > 0) boxes.push(r);
          }
        }
        if (boxes.length > 0) {
          glyphs = {
            left: Math.min(...boxes.map((r) => r.left)),
            right: Math.max(...boxes.map((r) => r.right)),
            top: Math.min(...boxes.map((r) => r.top)),
            bottom: Math.max(...boxes.map((r) => r.bottom)),
          };
        }
      } catch {
        glyphs = null;
      }
      const text = glyphs ?? H.rect(el);
      const edge = edgeBox;
      // Only sides with a drawn edge count: a borderless, paddingless,
      // same-as-behind side is open layout, not a container edge.
      const style = getComputedStyle(box);
      const back = behind(box);
      const fillEdge = opaque(style.backgroundColor ?? "") && style.backgroundColor !== back;
      const sideEdge = (side) =>
        H.px(style[`border${side}Width`]) > 0 || H.px(style[`padding${side}`]) > 0 || fillEdge;
      const gaps = {
        Left: text.left - edge.left,
        Right: edge.right - text.right,
        Top: text.top - edge.top,
        Bottom: edge.bottom - text.bottom,
      };
      const sides = {
        left_px: Math.round(gaps.Left * 10) / 10,
        right_px: Math.round(gaps.Right * 10) / 10,
        top_px: Math.round(gaps.Top * 10) / 10,
        bottom_px: Math.round(gaps.Bottom * 10) / 10,
      };
      const edged = ["Left", "Right", "Top", "Bottom"].filter((side) => sideEdge(side));
      if (edged.length === 0) continue;
      const inset = Math.round(Math.min(...edged.map((side) => gaps[side])) * 10) / 10;
      rows.push({
        selector: H.selectorOf(el),
        container: H.selectorOf(box),
        inset_px: inset,
        ...sides,
        text: (el.textContent ?? "").trim().slice(0, 60),
      });
      if (rows.length >= 400) break;
    }
    return {
      matched: rows.length,
      // A pixel of tolerance: a 0.x px miss of the floor is subpixel
      // rounding, not a visible defect. Glyphs past the edge (negative
      // inset) are overflow, which belongs to text-overflow.
      failures: rows
        .filter((row) => row.inset_px >= -1 && row.inset_px + 1 < MIN)
        .slice(0, 20),
    };
  });
  return verdict(ctx, rule, found.matched, found.failures, {
    min_inset_px: MIN_INSET_PX,
  });
}
