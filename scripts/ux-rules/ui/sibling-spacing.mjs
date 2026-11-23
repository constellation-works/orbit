// Siblings of one documented semantic kind share padding, font size and weight.
//
// The first class groups candidates. Compare matching class sets within that
// group and record mixed sets as ambiguous. Selected items are exempt.
import { measureInPage, verdict } from "../lib/ui-context.mjs";

export const rule = {
  id: "ui.sibling-spacing",
  title: "Siblings of one kind share spacing and type",
  severity: "low",
  surface: "ui",
  targets: { views: "*", widths: [1440, 375] },
};

export async function check(ctx) {
  const measured = await measureInPage(ctx, (H) => {
    const MIN_PEERS = 3;
    const STATE_TOKENS = new Set(["active", "selected"]);

    function tokensOf(el) {
      if (typeof el.className !== "string") return [];
      return el.className.trim().split(/\s+/).filter(Boolean);
    }

    // aria-selected and aria-pressed count only when true. aria-current is
    // any presence, matching the attribute the rule already honored.
    function selectedState(el) {
      if (el.getAttribute("aria-selected") === "true") return "aria-selected";
      if (el.getAttribute("aria-pressed") === "true") return "aria-pressed";
      if (el.hasAttribute("aria-current")) return "aria-current";
      for (const token of tokensOf(el)) if (STATE_TOKENS.has(token)) return token;
      return "";
    }

    function measuredItem(el) {
      const style = getComputedStyle(el);
      return {
        ...H.elementFacts(el),
        padding: style.padding,
        font_size: style.fontSize,
        font_weight: style.fontWeight,
      };
    }

    const groups = [];
    const coverage = [];

    for (const parent of H.all("*")) {
      const children = [...parent.children].filter((child) => H.visible(child));
      if (children.length < MIN_PEERS) continue;
      const clusters = new Map();
      for (const child of children) {
        const tokens = tokensOf(child);
        const bucket = child.tagName + "." + (tokens[0] ?? "");
        if (!clusters.has(bucket)) clusters.set(bucket, []);
        clusters.get(bucket).push(child);
      }
      for (const [bucket, members] of clusters) {
        if (members.length < MIN_PEERS) continue;
        const parentSelector = H.selectorOf(parent);
        const exempt = [];
        const rest = [];
        for (const el of members) {
          const state = selectedState(el);
          if (state) exempt.push({ selector: H.selectorOf(el), state });
          else rest.push(el);
        }
        if (exempt.length > 0) {
          coverage.push({
            parent: parentSelector,
            bucket,
            coverage: "state-exempt",
            members: exempt.slice(0, 12),
          });
        }
        if (rest.length < MIN_PEERS) continue;

        const profiles = rest.map((el) => ({ el, tokens: tokensOf(el).sort() }));
        const kinds = new Set(profiles.map((item) => item.tokens.join(".")));
        if (kinds.size === 1) {
          const kind = kinds.values().next().value || bucket;
          groups.push({
            parent: parentSelector,
            kind,
            bucket,
            items: rest.slice(0, 12).map(measuredItem),
          });
          coverage.push({
            parent: parentSelector,
            bucket,
            coverage: "compared",
            kind,
            count: rest.length,
          });
        } else {
          coverage.push({
            parent: parentSelector,
            bucket,
            coverage: "ambiguous",
            members: profiles.slice(0, 12).map(({ el, tokens }) => ({
              selector: H.selectorOf(el),
              tokens,
            })),
          });
        }
        if (groups.length >= 40) return { groups, coverage: coverage.slice(0, 40) };
      }
    }
    return { groups, coverage: coverage.slice(0, 40) };
  });

  let matched = 0;
  const failures = [];
  for (const group of measured.groups) {
    matched += group.items.length;
    const modal = (key) => [...group.items].sort((a, b) =>
      group.items.filter((item) => item[key] === b[key]).length
      - group.items.filter((item) => item[key] === a[key]).length)[0][key];
    for (const key of ["padding", "font_size", "font_weight"]) {
      const expected = modal(key);
      for (const item of group.items) {
        if (item[key] === expected) continue;
        failures.push({
          selector: item.selector,
          element: item.element,
          discriminator: key,
          parent: group.parent,
          kind: group.kind,
          property: key,
          value: item[key],
          sibling_value: expected,
        });
      }
    }
  }
  return verdict(ctx, rule, matched, failures, { coverage: measured.coverage },
    measured.groups.flatMap((group) => group.items), ["padding", "font_size", "font_weight"]);
}
