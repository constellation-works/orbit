// Small inline SVG line charts shared by dashboard views. Nodes are built
// with DOM calls only, never markup strings.

const SVG_NS = "http://www.w3.org/2000/svg";

export function svg(tag, attrs = {}) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, String(value));
  return node;
}

/// An SVG path through `points` (0–100 space), broken at nulls. A lone point
/// gets a zero-length segment so its round cap still shows.
export function linePath(points) {
  let d = "";
  let run = 0;
  for (const point of points) {
    if (!point) {
      if (run === 1) d += "l0.01 0 ";
      run = 0;
      continue;
    }
    d += `${run ? "L" : "M"}${(point.x * 10).toFixed(1)} ${(point.y * 10).toFixed(1)} `;
    run += 1;
  }
  if (run === 1) d += "l0.01 0";
  return d.trim();
}

/// A line through `points` (0–100 space, y down) stretched over its box,
/// labelled `label` for assistive technology, or hidden from it when the
/// caller labels the chart around it. `extra` SVG nodes, in the same 0–1000
/// viewBox, are drawn beneath the line.
export function sparkline(points, { className, lineClass, label, extra = [] }) {
  const named = label ? { role: "img", "aria-label": label } : { "aria-hidden": "true" };
  const node = svg("svg", { viewBox: "0 0 1000 1000", preserveAspectRatio: "none", class: className, ...named });
  for (const child of extra) node.appendChild(child);
  node.appendChild(svg("path", { d: linePath(points), class: lineClass, "vector-effect": "non-scaling-stroke" }));
  return node;
}
