// Text reaches 4.5:1 (3:1 large) against its effective background.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.contrast", title: "Text contrast meets 4.5:1, 3:1 for large text", severity: "medium", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const m = await measureInPage(ctx, (H) => {
    const parse = (value) => {
      const match = String(value).match(/rgba?\(([^)]+)\)/);
      if (!match) return null;
      const parts = match[1].split(",").map((part) => Number.parseFloat(part));
      return { r: parts[0], g: parts[1], b: parts[2], a: parts[3] === undefined ? 1 : parts[3] };
    };
    const blend = (fg, bg) => ({ r: fg.r * fg.a + bg.r * (1 - fg.a), g: fg.g * fg.a + bg.g * (1 - fg.a), b: fg.b * fg.a + bg.b * (1 - fg.a), a: 1 });
    const luminance = (c) => { const f = (v) => { const s = v / 255; return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4); }; return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b); };
    const effectiveBg = (el) => {
      let node = el; const layers = [];
      while (node && node !== document.documentElement) {
        const s = getComputedStyle(node);
        if (s.backgroundImage && s.backgroundImage !== "none") return null;
        const bg = parse(s.backgroundColor);
        if (bg && bg.a > 0) layers.push(bg);
        if (bg && bg.a === 1) break;
        node = node.parentElement;
      }
      let base = { r: 255, g: 255, b: 255, a: 1 };
      for (const layer of layers.reverse()) base = blend(layer, base);
      return base;
    };
    let matched = 0; let unmeasurable = 0; const failures = [];
    for (const el of H.all("*")) {
      const ownText = [...el.childNodes].some((node) => node.nodeType === 3 && node.textContent.trim() !== "");
      if (!ownText) continue;
      if (matched >= 300) break;
      const s = getComputedStyle(el);
      if (H.px(s.fontSize) <= 0) continue;
      const fg = parse(s.color); const bg = effectiveBg(el);
      if (!fg || !bg) { unmeasurable += 1; continue; }
      matched += 1;
      const blended = fg.a < 1 ? blend(fg, bg) : fg;
      const l1 = luminance(blended); const l2 = luminance(bg);
      const ratio = (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
      const size = H.px(s.fontSize); const weight = Number.parseInt(s.fontWeight, 10) || 400;
      const large = size >= 24 || (size >= 18.66 && weight >= 700);
      const needed = large ? 3 : 4.5;
      if (ratio < needed) failures.push({ selector: H.selectorOf(el), ratio: Math.round(ratio * 100) / 100, needed, font_size: s.fontSize, text: (el.textContent ?? "").trim().slice(0, 60) });
      if (failures.length >= 20) break;
    }
    return { matched, unmeasurable, failures };
  });
  return verdict(ctx, rule, m.matched, m.failures, { unmeasurable: m.unmeasurable });
}
