// Neighbouring columns' first comparable content starts at the same top.
//
// The start is the first visible text line in document order, so a column's
// own text before a nested child counts and a wrapped line after that text
// does not. A control (button, field, or replaced graphic) contributes its
// own visible start. Text starts are compared only with text starts, and
// control starts only with control starts: mixing a heading line with a
// centred control, or a wrapper edge with a nested child, is not a
// measurement. An anchor with no peer of its kind, no rendered content, or
// no geometry is recorded and is not counted as a pass.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.aligned-starts",
  title: "Neighbouring columns start at the same top",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const ROW_TOP_PX = 4;
    const START_TOLERANCE_PX = 8;
    const FIELD = new Set(["INPUT", "SELECT", "TEXTAREA"]);
    const GRAPHIC = new Set(["IMG", "SVG", "CANVAS", "VIDEO", "IFRAME"]);

    // Screen-reader text is laid out as a 1px clipped box. It is not a
    // column start, and measuring it beside a neighbour's real text is
    // the host-chip and hint false positive.
    function assistiveOnly(el) {
      const style = getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") return true;
      const rect = el.getBoundingClientRect();
      if (rect.width <= 1 && rect.height <= 1) return true;
      const clipped = style.overflow === "hidden" || style.overflowX === "hidden" || style.overflowY === "hidden";
      return rect.width <= 2 && rect.height <= 2 && clipped;
    }

    function kindOf(el) {
      if (el.tagName === "BUTTON" || FIELD.has(el.tagName) || GRAPHIC.has(el.tagName)) return "control";
      if (el.closest("button, select, textarea, input")) return "control";
      return "text";
    }

    function firstLine(node) {
      let rects = [];
      try {
        const range = document.createRange();
        range.selectNodeContents(node);
        rects = [...range.getClientRects()];
      } catch {
        return null;
      }
      const usable = rects.filter((rect) => rect.width > 0.5 && rect.height > 0.5);
      if (usable.length === 0) return null;
      let best = usable[0];
      for (const rect of usable) if (rect.top < best.top - 0.01) best = rect;
      return { top: best.top, width: best.width };
    }

    // Native fields and graphics have no measurable text node. A field's
    // glyphs sit in the centred line box; a graphic starts at its border box.
    function replacedStart(el) {
      const style = getComputedStyle(el);
      const rect = el.getBoundingClientRect();
      if (rect.width < 2 || rect.height < 2) return null;
      const borderTop = H.px(style.borderTopWidth);
      const borderBottom = H.px(style.borderBottomWidth);
      const padTop = H.px(style.paddingTop);
      const padBottom = H.px(style.paddingBottom);
      let top = rect.top;
      if (FIELD.has(el.tagName)) {
        const contentTop = rect.top + borderTop + padTop;
        const contentHeight = Math.max(0, rect.height - borderTop - borderBottom - padTop - padBottom);
        const fontSize = H.px(style.fontSize) || 16;
        let line = fontSize * 1.2;
        if (style.lineHeight.endsWith("px")) line = H.px(style.lineHeight);
        else if (style.lineHeight !== "normal" && Number(style.lineHeight) > 0) line = fontSize * Number(style.lineHeight);
        top = contentTop + Math.max(0, (contentHeight - line) / 2);
      }
      const raw = FIELD.has(el.tagName)
        ? el.value || el.placeholder || el.getAttribute("aria-label") || ""
        : el.getAttribute("aria-label") || "";
      return {
        element: el,
        top,
        width: rect.width,
        text: String(raw).trim().replace(/\s+/g, " ").slice(0, 80),
        kind: "control",
      };
    }

    function contentStart(el) {
      if (!(el instanceof Element) || !H.visible(el) || assistiveOnly(el)) return null;
      if (el.tagName === "SCRIPT" || el.tagName === "STYLE" || el.tagName === "NOSCRIPT" || el.tagName === "TEMPLATE") return null;
      if (FIELD.has(el.tagName) || GRAPHIC.has(el.tagName)) return replacedStart(el);
      for (const node of el.childNodes) {
        if (node.nodeType === 3) {
          if (node.textContent.trim() === "") continue;
          const line = firstLine(node);
          // Missing geometry must not fall through to a later child or the
          // wrapper edge: that is the measurement that manufactured the
          // false starts.
          if (!line) return { unavailable: true, element: el, reason: "content geometry unavailable" };
          return {
            element: el,
            top: line.top,
            width: line.width,
            text: node.textContent.trim().replace(/\s+/g, " ").slice(0, 80),
            kind: kindOf(el),
          };
        }
        if (node.nodeType === 1) {
          const found = contentStart(node);
          if (found) return found;
        }
      }
      return null;
    }

    function anchorOf(start, column) {
      return {
        node: start.element,
        selector: H.selectorOf(start.element),
        column: H.selectorOf(column),
        top_px: Math.round(start.top),
        width_px: Math.round(start.width),
        text: start.text,
        kind: start.kind,
      };
    }

    const comparisons = [];
    const incomparable = [];
    const failures = [];
    for (const parent of H.all("*")) {
      if (comparisons.length >= 30) break;
      const style = getComputedStyle(parent);
      const isRow = style.display === "grid" || (style.display === "flex" && style.flexDirection.startsWith("row"));
      if (!isRow) continue;
      const children = [...parent.children].filter((child) => H.visible(child));
      if (children.length < 2) continue;
      const tops = children.map((child) => Math.round(H.rect(child).top));
      if (Math.max(...tops) - Math.min(...tops) > ROW_TOP_PX) continue;

      const byKind = new Map();
      const rowUncompared = [];
      let comparable = false;
      for (const child of children) {
        const start = contentStart(child);
        if (!start) {
          rowUncompared.push({
            parent: H.selectorOf(parent),
            column_selector: H.selectorOf(child),
            reason: "no comparable rendered content",
          });
          continue;
        }
        if (start.unavailable) {
          rowUncompared.push({
            parent: H.selectorOf(parent),
            column_selector: H.selectorOf(child),
            selector: H.selectorOf(start.element),
            reason: start.reason,
          });
          continue;
        }
        const anchor = anchorOf(start, child);
        const group = byKind.get(anchor.kind) ?? [];
        group.push(anchor);
        byKind.set(anchor.kind, group);
      }

      const rowAnchors = [];
      for (const group of byKind.values()) {
        if (group.length < 2) {
          for (const anchor of group) {
            rowUncompared.push({
              parent: H.selectorOf(parent),
              column_selector: anchor.column,
              selector: anchor.selector,
              top_px: anchor.top_px,
              text: anchor.text,
              kind: anchor.kind,
              reason: "no comparable peer",
            });
          }
          continue;
        }
        comparable = true;
        const peer = group.reduce((best, item) => (item.top_px < best.top_px ? item : best));
        for (const item of group) {
          const delta = item.top_px - peer.top_px;
          const recorded = {
            parent: H.selectorOf(parent),
            selector: item.selector,
            anchor_selector: item.selector,
            peer_selector: peer.selector,
            column_selector: item.column,
            peer_column_selector: peer.column,
            anchor_top_px: item.top_px,
            peer_top_px: peer.top_px,
            delta_px: delta,
            text: item.text,
            peer_text: peer.text,
            width_px: item.width_px,
            kind: item.kind,
            node: item.node,
          };
          rowAnchors.push(recorded);
          if (delta > START_TOLERANCE_PX) failures.push(recorded);
        }
      }
      if (!comparable && rowUncompared.length === 0) continue;
      if (comparable) {
        comparisons.push({
          parent: H.selectorOf(parent),
          anchors: rowAnchors.map((item) => ({
            selector: item.selector,
            column_selector: item.column_selector,
            peer_selector: item.peer_selector,
            anchor_top_px: item.anchor_top_px,
            peer_top_px: item.peer_top_px,
            delta_px: item.delta_px,
            text: item.text,
            peer_text: item.peer_text,
            width_px: item.width_px,
            kind: item.kind,
          })),
        });
      }
      for (const item of rowUncompared) {
        if (incomparable.length < 30) incomparable.push(item);
      }
    }

    const reported = failures.slice(0, 20).map((failure, index) => {
      const handle = `ui.aligned-starts-${index}`;
      failure.node?.setAttribute("data-ux-evidence-target", handle);
      const { node, ...serial } = failure;
      return { ...serial, evidence_selector: `[data-ux-evidence-target="${handle}"]` };
    });
    const matched = comparisons.reduce((sum, row) => sum + row.anchors.length, 0);
    return { matched, failures: reported, comparisons, incomparable };
  });

  const extra = { comparisons: found.comparisons, incomparable: found.incomparable };
  if (found.matched === 0) {
    extra.reason = found.incomparable.length > 0 ? "no comparable content starts" : "no element matched on this target";
  }
  return verdict(ctx, rule, found.matched, found.failures, extra);
}
