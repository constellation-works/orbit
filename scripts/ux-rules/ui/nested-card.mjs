// No bordered or filled box directly inside a bordered panel without a grouping
// role or label. A full-bleed panel band is not a card: measured on the
// visual fixtures at 1440 and 375, the false positives (#agents-controls,
// #doctor-controls, #clock-body, #routines-body, and the same shape on other
// views) sit flush with the panel content box, have no margin, and carry no
// side border, radius, shadow or outline. A top or bottom hairline is a
// section divider. Inset, separated or framed boxes still fail, including a
// full-width box that is actually a card.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.nested-card",
  title: "No card nested directly inside a panel",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

const ALLOW = new Set(["TABLE", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "A", "CODE", "PRE", "svg"]);

export async function check(ctx) {
  const found = await measureInPage(ctx, (H) => {
    const sideOn = (style, name) => H.px(style[`border${name}Width`]) > 0 && style[`border${name}Style`] !== "none";
    const bordered = (el) => sideOn(getComputedStyle(el), "Top");
    const filled = (el) => {
      const background = getComputedStyle(el).backgroundColor;
      return background !== "rgba(0, 0, 0, 0)" && background !== "transparent";
    };
    // Flush within 1px: the same subpixel tolerance as the other UI rules.
    const near = (value) => Math.abs(value) <= 1;
    const result = [];
    for (const panel of H.all(".panel")) {
      if (!bordered(panel)) continue;
      const panelStyle = getComputedStyle(panel);
      const panelRect = panel.getBoundingClientRect();
      const contentLeft = panelRect.left + H.px(panelStyle.borderLeftWidth) + H.px(panelStyle.paddingLeft);
      const contentRight = panelRect.right - H.px(panelStyle.borderRightWidth) - H.px(panelStyle.paddingRight);
      for (const child of [...panel.children]) {
        if (!H.visible(child)) continue;
        const style = getComputedStyle(child);
        const rect = child.getBoundingClientRect();
        const margins = ["Top", "Right", "Bottom", "Left"].map((side) => H.px(style[`margin${side}`]));
        const radius = ["TopLeft", "TopRight", "BottomRight", "BottomLeft"].some((corner) => H.px(style[`border${corner}Radius`]) > 0);
        const shadow = Boolean(style.boxShadow) && style.boxShadow !== "none";
        const outline = Boolean(style.outlineStyle) && style.outlineStyle !== "none";
        const framed = sideOn(style, "Left") || sideOn(style, "Right") || radius || shadow || outline;
        const flow = style.position === "static" || style.position === "relative";
        const flush = near(rect.left - contentLeft) && near(rect.right - contentRight);
        const stacked = margins.every((value) => near(value));
        result.push({
          panel: H.selectorOf(panel),
          selector: H.selectorOf(child),
          tag: child.tagName,
          role: child.getAttribute("role") ?? "",
          bordered: bordered(child),
          filled: filled(child),
          labelled: child.hasAttribute("aria-label") || child.hasAttribute("aria-labelledby"),
          band: flush && stacked && flow && !framed,
        });
      }
    }
    return result;
  });
  const failures = found.filter((item) => (item.bordered || item.filled) && item.band !== true && !ALLOW.has(item.tag) && !item.role && !item.labelled);
  return verdict(ctx, rule, found.length, failures);
}
