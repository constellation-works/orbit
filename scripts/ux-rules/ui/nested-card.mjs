// No bordered or filled box directly inside a bordered panel, or inside a
// task-detail disclosure, description panel or delivery card, without a
// grouping role or label. A full-bleed band is not a card: measured on the
// visual fixtures at 1440 and 375, the false positives (#agents-controls,
// #doctor-controls, #clock-body, #routines-body, and the same shape on other
// views) sit flush with the container content box, have no margin, and carry
// no side border, radius, shadow or outline. A top or bottom hairline is a
// section divider. Inset, separated or framed boxes still fail, including a
// full-width box that is actually a card. A floated box is out of normal flow
// and still fails, even when it is flush with the content box. A float does
// not apply to a flex or grid item, so there it stays in flow.
//
// Task detail renders no `.panel`. Its disclosure rows live in a bordered
// `.detail-disclosures` group (the first row often has no border of its own),
// and a task with no folded sections still has a bordered description panel
// and delivery card. Those are measured too. The same band, float and margin
// exemptions apply, so a hairline property row is not a nested card.
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
    const seen = new Set();
    // Task detail has no `.panel`. Disclosure rows take their border from the
    // group when the row itself has none. The description panel and delivery
    // card are the bordered containers on a detail that has no disclosures.
    const containers = [
      ...H.all(".panel"),
      ...H.all(".task-detail .detail-disclosures > .field-block"),
      ...H.all(".task-detail .task-markdown-panel"),
      ...H.all(".task-detail .task-delivery-card"),
    ];
    for (const panel of containers) {
      if (seen.has(panel)) continue;
      seen.add(panel);
      const parent = panel.parentElement;
      const disclosure = Boolean(
        parent && parent.classList.contains("detail-disclosures") && panel.classList.contains("field-block"),
      );
      if (!bordered(panel) && !(disclosure && bordered(parent))) continue;
      const panelStyle = getComputedStyle(panel);
      const panelRect = panel.getBoundingClientRect();
      // CSS ignores float on flex and grid items, yet the computed value stays.
      const floatInert = ["flex", "inline-flex", "grid", "inline-grid"].includes(panelStyle.display);
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
        // A float keeps position: static but is out of normal flow, so it never
        // counts as a band, unless the panel is a flex or grid container, where
        // the float does not apply and the child stays in flow.
        const floated = style.float !== "none" && !floatInert;
        const flow = (style.position === "static" || style.position === "relative") && !floated;
        const flush = near(rect.left - contentLeft) && near(rect.right - contentRight);
        const stacked = margins.every((value) => near(value));
        result.push({
          panel: H.selectorOf(panel),
          ...H.elementFacts(child),
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
  return verdict(ctx, rule, found.length, failures, {}, found);
}
