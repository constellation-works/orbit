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

    // A clip-path can leave a box with full layout size and no painted area,
    // so its text is not a start. Parses a length to px; a percentage resolves
    // against the given size. Anything else is NaN.
    function clipLength(token, size) {
      const match = /^(-?\d*\.?\d+)(px|%)?$/.exec(token);
      if (!match) return Number.NaN;
      const value = Number(match[1]);
      return match[2] === "%" ? (value / 100) * size : value;
    }

    // An inset whose opposing offsets reach the box size on an axis covers
    // it. A circle or ellipse with no radius has no area. Shapes and units
    // this cannot resolve, such as url() or calc(), keep the content.
    function clipPathPaintsNothing(el) {
      const clip = getComputedStyle(el).clipPath;
      if (!clip || clip === "none") return false;
      const inset = /^inset\((.*)\)$/.exec(clip);
      if (inset) {
        const rect = el.getBoundingClientRect();
        const [top, right = top, bottom = top, left = right] = inset[1].split(/\s+round\s+/)[0].trim().split(/\s+/);
        const vertical = [clipLength(top, rect.height), clipLength(bottom, rect.height)];
        const horizontal = [clipLength(left, rect.width), clipLength(right, rect.width)];
        if ([...vertical, ...horizontal].some(Number.isNaN)) return false;
        return vertical[0] + vertical[1] >= rect.height - 0.5 || horizontal[0] + horizontal[1] >= rect.width - 0.5;
      }
      const shape = /^(circle|ellipse)\((.*)\)$/.exec(clip);
      if (shape) {
        const radii = shape[2].split(/\s+at\s+/)[0].trim().split(/\s+/);
        return radii.every((token) => clipLength(token, 1) === 0);
      }
      return false;
    }

    // A clip-path clips its whole subtree, including a column above the one
    // being measured, so the walk runs to the root.
    function hiddenByClip(el) {
      for (let node = el; node instanceof Element; node = node.parentElement) {
        if (clipPathPaintsNothing(node)) return true;
      }
      return false;
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
      return { top: best.top, width: best.width, height: best.height };
    }

    function usedLine(style) {
      const fontSize = H.px(style.fontSize) || 16;
      let line = fontSize * 1.2;
      if (style.lineHeight.endsWith("px")) line = H.px(style.lineHeight);
      else if (style.lineHeight !== "normal" && Number(style.lineHeight) > 0) line = fontSize * Number(style.lineHeight);
      return line;
    }

    // A zero font size or a zero line height has no line box to place.
    function lineBoxMeasurable(style) {
      if (H.px(style.fontSize) <= 0) return false;
      if (style.lineHeight.endsWith("px")) return H.px(style.lineHeight) > 0.5;
      if (style.lineHeight !== "normal") {
        const factor = Number(style.lineHeight);
        if (Number.isFinite(factor) && factor <= 0) return false;
      }
      return true;
    }

    // Only a px or unitless line-height gives the used line height exactly.
    // `normal` depends on font metrics this measurement does not read, so a
    // scrolled position cannot be placed on a line boundary from it.
    function lineHeightEstablished(style) {
      if (style.lineHeight.endsWith("px")) return H.px(style.lineHeight) > 0.5;
      if (style.lineHeight === "normal") return false;
      const factor = Number(style.lineHeight);
      return Number.isFinite(factor) && factor > 0;
    }

    // A closed select and a text input centre one line. A textarea always
    // stacks lines from the top, and so does a select shown as a list.
    function multilineField(el) {
      if (el.tagName === "TEXTAREA") return true;
      return el.tagName === "SELECT" && (el.multiple || el.size > 1);
    }

    // Ignore a rect as tall as the control: that is the border box, not a line.
    function measuredFieldLine(el, rect) {
      const line = firstLine(el);
      if (!line || !(line.height > 0.5)) return null;
      if (rect.height > 48 && line.height > rect.height * 0.6) return null;
      return line;
    }

    // Native fields and graphics have no author text node for a single line.
    // A single-line field centres that line. A multiline field's first line
    // starts at the content top; a graphic starts at its border box.
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
        if (multilineField(el)) {
          if (el.tagName === "TEXTAREA") {
            // A textarea does not expose line rects. Its original first line
            // is hidden above the clip once scrolled, so measuring it (the
            // content top minus all of scrollTop) invents a start nothing
            // paints. Measure instead the first line wholly inside the
            // content clip: after a whole number of lines that line sits at
            // the content top again, whatever scrollTop is.
            if (!lineBoxMeasurable(style) || !Number.isFinite(el.scrollTop)) {
              return { unavailable: true, element: el, reason: "content geometry unavailable" };
            }
            const contentTop = rect.top + borderTop + padTop;
            const scrollTop = Math.max(0, el.scrollTop);
            if (scrollTop <= 0) {
              top = contentTop;
            } else {
              if (!lineHeightEstablished(style)) {
                return { unavailable: true, element: el, reason: "content geometry unavailable" };
              }
              const line = usedLine(style);
              const contentHeight = Math.max(0, rect.height - borderTop - borderBottom - padTop - padBottom);
              if (!(line > 0.5) || contentHeight <= 0.5) {
                return { unavailable: true, element: el, reason: "content geometry unavailable" };
              }
              const offset = (line - (scrollTop % line)) % line;
              if (offset + line > contentHeight) {
                return { unavailable: true, element: el, reason: "content geometry unavailable" };
              }
              top = contentTop + offset;
            }
          } else {
            const measured = measuredFieldLine(el, rect);
            if (measured) {
              top = measured.top;
            } else {
              return { unavailable: true, element: el, reason: "content geometry unavailable" };
            }
          }
        } else {
          const contentTop = rect.top + borderTop + padTop;
          const contentHeight = Math.max(0, rect.height - borderTop - borderBottom - padTop - padBottom);
          top = contentTop + Math.max(0, (contentHeight - usedLine(style)) / 2);
        }
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
      if (!(el instanceof Element) || !H.visible(el) || assistiveOnly(el) || hiddenByClip(el)) return null;
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
        ...H.elementFacts(start.element),
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
    const rows = [];
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
            element: item.element,
            node: item.node,
          };
          rowAnchors.push(recorded);
          rows.push(recorded);
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
    // Every compared anchor is an observation; failures past the reported
    // twenty keep their identity so they never read as passing members.
    const strip = ({ node, ...serial }) => serial;
    return { matched, failures: reported, all_failures: failures.map(strip), rows: rows.map(strip), comparisons, incomparable };
  });

  const extra = { comparisons: found.comparisons, incomparable: found.incomparable };
  if (found.matched === 0) {
    extra.reason = found.incomparable.length > 0 ? "no comparable content starts" : "no element matched on this target";
  }
  return verdict(ctx, rule, found.matched, found.failures, extra, found.rows, ["start"], found.all_failures);
}
