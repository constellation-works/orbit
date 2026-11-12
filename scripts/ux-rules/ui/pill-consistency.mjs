// Live-state pills that play one role share height, horizontal padding, font
// size, font weight and border radius (STD-07 R30: one kind of element looks
// the same everywhere).
//
// A pill is an element with text that is single-line (one client rect, its text
// on one line, height at most 40px), has a small font (at most 14px), a border
// radius of at least half its height, and a painted border or background. A
// nearly-round box (radius at least 4px and 0.3x its height) is a loose
// candidate: it is compared only inside a group that holds a true pill, so a
// square-cornered badge among round pills fails on border-radius. The
// markup is not consulted, so a `span.chip`, an `a.chip` and a
// `.delivery-attention-badge` are all pills when they look like one.
//
// Filter chips are a different kind and are never candidates: a pill that is a
// button, a form control, a tab/radio/switch/option, carries `aria-pressed`,
// `aria-selected` or `aria-checked`, or sits inside a toolbar, tablist, radio
// group, `.chip-group` or an element whose class names a filter, is skipped.
// A link or a plain span is a status pill (the `triaging` chip is a link).
//
// Grouping heuristic (no marker is required): the pills of one row form a
// group, and the pills of the rows of one repeated list or table form another.
// A row is the nearest ancestor that has at least one sibling of the same tag
// and first class (a `tr`, `li` or `.row`); its parent is the list. A pill with
// no repeated ancestor is grouped with the pills sharing its parent (a card
// header, a detail field). Within a group of two or more pills, the modal
// value of each property is the reference (ties go to the value met first in
// document order) and a pill differing from it fails: height by more than 2px;
// padding-left, padding-right or font size by more than half a pixel; font
// weight at all; or border roundness by more than 0.03. Roundness is the radius
// capped at half the height, over the height, so a 999px radius and a
// half-height radius are the same shape; the failure reports the radius in px.
//
// An opt-out for a deliberate difference is `data-ux-pill="<kind>"`: pills
// carrying different kinds are never compared with each other, and pills of one
// kind are compared only among themselves.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.pill-consistency", title: "Pills in one list or row share size and type", severity: "low", surface: "ui", targets: { views: "*", widths: [1440, 375] } };

const PROPERTIES = [
  { property: "height", key: "height_px", tolerance: 2 },
  { property: "padding-left", key: "padding_left_px", tolerance: 0.5 },
  { property: "padding-right", key: "padding_right_px", tolerance: 0.5 },
  { property: "font-size", key: "font_size_px", tolerance: 0.5 },
  { property: "font-weight", key: "font_weight", tolerance: 0 },
  { property: "border-radius", key: "radius_ratio", tolerance: 0.03, report: "radius_px" },
];

export async function check(ctx) {
  const groups = await measureInPage(ctx, (H) => {
    const INTERACTIVE = "button, input, select, textarea, summary, [role=button], [role=tab], [role=radio], [role=switch], [role=option], [role=menuitem], [aria-pressed], [aria-selected], [aria-checked]";
    const FILTER_BAR = "[role=toolbar], [role=tablist], [role=radiogroup], .chip-group, [class*=filter], [id*=filter]";
    const alpha = (color) => {
      const m = /rgba?\(([^)]+)\)/.exec(color);
      if (!m) return color && color !== "transparent" ? 1 : 0;
      const parts = m[1].split(/[\s,/]+/).filter(Boolean);
      return parts.length >= 4 ? Number.parseFloat(parts[3]) || 0 : 1;
    };
    const facts = (el) => {
      const s = getComputedStyle(el);
      const r = H.rect(el);
      const fontSize = H.px(s.fontSize);
      if (!(fontSize > 0 && fontSize <= 14) || r.height < 8 || r.height > 40 || r.width <= r.height * 0.9) return null;
      if (el.getClientRects().length !== 1 || !(el.textContent || "").trim() || (el.textContent || "").trim().length > 48) return null;
      const range = document.createRange();
      range.selectNodeContents(el);
      const tops = [...range.getClientRects()].filter((q) => q.width > 0 && q.height > 0).map((q) => q.top);
      if (tops.length && Math.max(...tops) - Math.min(...tops) > fontSize * 0.8) return null;
      if (el.querySelector("p, div, ul, ol, table, li, input, select, textarea, button")) return null;
      const radii = ["borderTopLeftRadius", "borderTopRightRadius", "borderBottomLeftRadius", "borderBottomRightRadius"].map((k) => H.px(s[k]));
      const radius = Math.min(...radii.map((v) => Math.min(v, r.height / 2)));
      // A pill is fully rounded; a nearly-rounded box (radius at least 0.3x its
      // height) is a loose candidate that counts only beside a strict pill.
      const strict = radius >= r.height / 2 - 0.5;
      if (!strict && !(radius >= 4 && radius >= r.height * 0.3)) return null;
      const border = ["Top", "Right", "Bottom", "Left"].some((side) => H.px(s["border" + side + "Width"]) > 0 && s["border" + side + "Style"] !== "none" && alpha(s["border" + side + "Color"]) > 0);
      if (!border && alpha(s.backgroundColor) === 0) return null;
      return { strict, height_px: Math.round(r.height * 10) / 10, padding_left_px: H.px(s.paddingLeft), padding_right_px: H.px(s.paddingRight), font_size_px: fontSize, font_weight: String(s.fontWeight), radius_px: Math.round(radius * 10) / 10, radius_ratio: Math.round((radius / r.height) * 100) / 100, top: Math.round(r.top), width_px: Math.round(r.width) };
    };
    // Innermost pill only: an outer pill wrapping a pill is one pill.
    const pills = [];
    for (const el of H.all("*")) {
      if (el.matches(INTERACTIVE) || el.closest(INTERACTIVE) || el.closest(FILTER_BAR)) continue;
      const f = facts(el);
      // The document-order index is the pill's identity: selector, top and
      // width are not unique (two triaging chips in one row share all three).
      if (f) pills.push({ el, f, index: pills.length });
    }
    const pillSet = new Set(pills.map((p) => p.el));
    const innermost = pills.filter((p) => ![...p.el.querySelectorAll("*")].some((c) => pillSet.has(c)));
    const repeated = (el) => {
      const parent = el.parentElement;
      if (!parent) return false;
      const cls = (n) => (typeof n.className === "string" ? n.className.trim().split(/\s+/)[0] || "" : "");
      return [...parent.children].filter((s) => s !== el && s.tagName === el.tagName && cls(s) === cls(el)).length > 0;
    };
    const rowOf = (el) => {
      for (let n = el.parentElement; n && n !== document.body; n = n.parentElement) if (repeated(n)) return n;
      return null;
    };
    const buckets = new Map();
    const add = (key, scope, owner, p) => {
      if (!buckets.has(key)) buckets.set(key, { scope, owner, items: [] });
      buckets.get(key).items.push(p);
    };
    const ids = new Map();
    const idOf = (n) => { if (!ids.has(n)) ids.set(n, ids.size); return ids.get(n); };
    for (const p of innermost) {
      const kind = p.el.closest("[data-ux-pill]")?.getAttribute("data-ux-pill") || "";
      const row = rowOf(p.el);
      const item = { ...H.elementFacts(p.el), kind, ...p.f, pill_index: p.index };
      if (row) {
        add("row|" + idOf(row) + "|" + kind, "row", H.selectorOf(row), item);
        add("list|" + idOf(row.parentElement) + "|" + kind, "list", H.selectorOf(row.parentElement), item);
      } else {
        add("parent|" + idOf(p.el.parentElement) + "|" + kind, "parent", H.selectorOf(p.el.parentElement), item);
      }
    }
    return [...buckets.values()].filter((g) => g.items.length >= 2 && g.items.some((i) => i.strict)).slice(0, 80);
  });
  let matched = 0; const failures = []; const observed = []; const seen = new Set(); const failed = new Set();
  for (const group of groups) {
    const items = group.items;
    // One element in a row group and a list group counts once; distinct pills never share an index.
    for (const item of items) if (!seen.has(item.pill_index)) { seen.add(item.pill_index); matched += 1; observed.push(item); }
    // Modal value within tolerance; a tie keeps the value met first.
    const modalOf = ({ key, tolerance }) => {
      let best = items[0];
      let bestCount = 0;
      for (const candidate of items) {
        const count = items.filter((i) => Math.abs(i[key] - candidate[key]) <= tolerance).length;
        if (count > bestCount) { best = candidate; bestCount = count; }
      }
      return best;
    };
    for (const spec of PROPERTIES) {
      const reference = modalOf(spec);
      for (const item of items) {
        if (item === reference || Math.abs(item[spec.key] - reference[spec.key]) <= spec.tolerance) continue;
        // One pill can sit in a row group and a list group; report it once.
        const identity = [item.pill_index, spec.property].join("|");
        if (failed.has(identity)) continue;
        failed.add(identity);
        failures.push({
          ...item,
          discriminator: spec.property,
          property: spec.property,
          group: group.scope,
          container: group.owner,
          value: item[spec.report ?? spec.key],
          sibling_selector: reference.selector,
          sibling_value: reference[spec.report ?? spec.key],
        });
      }
    }
  }
  return verdict(ctx, rule, matched, failures, {}, observed, PROPERTIES.map((p) => p.property));
}
