// Buttons, links, inputs and images have an accessible name.
import { measureInPage, verdict } from "../lib/ui-context.mjs";
export const rule = { id: "ui.accessible-name", title: "Interactive elements and images have an accessible name", severity: "medium", surface: "ui", targets: { views: "*", widths: [1440, 375] } };
export async function check(ctx) {
  const items = await measureInPage(ctx, (H) => {
    const nameOf = (el) => {
      const labelledBy = el.getAttribute("aria-labelledby");
      if (labelledBy) { const text = labelledBy.split(/\s+/).map((id) => document.getElementById(id)?.textContent ?? "").join(" ").trim(); if (text) return text; }
      if ((el.getAttribute("aria-label") ?? "").trim()) return el.getAttribute("aria-label").trim();
      if (el.tagName === "IMG") return (el.getAttribute("alt") ?? "").trim();
      if (el.id) { const label = document.querySelector('label[for="' + CSS.escape(el.id) + '"]'); if (label?.textContent.trim()) return label.textContent.trim(); }
      const wrappingLabel = el.closest("label");
      if (wrappingLabel?.textContent.trim()) return wrappingLabel.textContent.trim();
      if ((el.textContent ?? "").trim()) return el.textContent.trim();
      if ((el.getAttribute("title") ?? "").trim()) return el.getAttribute("title").trim();
      if ((el.getAttribute("placeholder") ?? "").trim()) return el.getAttribute("placeholder").trim();
      return "";
    };
    return H.all("button, a[href], input, select, textarea, img").map((el) => ({
      selector: H.selectorOf(el), tag: el.tagName.toLowerCase(),
      type: el.getAttribute("type") ?? "",
      hiddenInput: el.tagName === "INPUT" && el.getAttribute("type") === "hidden",
      decorative: el.tagName === "IMG" && el.hasAttribute("alt") && el.getAttribute("alt") === "",
      ariaHidden: el.getAttribute("aria-hidden") === "true",
      name: nameOf(el).slice(0, 80),
    }));
  });
  const relevant = items.filter((item) => !item.hiddenInput && !item.ariaHidden);
  const failures = relevant.filter((item) => item.name === "" && !item.decorative);
  return verdict(ctx, rule, relevant.length, failures);
}
