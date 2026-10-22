// Draws the wheel as SVG. Geometry matches core/src/wheel.rs (logical pixels).
const OUTER = 140;
const INNER = 84;
const PADDING = 12;
const SIZE = 2 * (OUTER + PADDING);
const C = SIZE / 2;
const SVG_NS = "http://www.w3.org/2000/svg";

const svg = document.getElementById("wheel");
svg.setAttribute("viewBox", `0 0 ${SIZE} ${SIZE}`);

let content = window.__FLICK_WHEEL__ || { slices: [], caption: "" };
let highlighted = null;

function angleOf(index, count) {
  return -Math.PI / 2 + (index * 2 * Math.PI) / count;
}

function at(r, a) {
  return [C + r * Math.cos(a), C + r * Math.sin(a)];
}

function segmentPath(from, to) {
  const large = to - from > Math.PI ? 1 : 0;
  const [x0, y0] = at(OUTER, from);
  const [x1, y1] = at(OUTER, to);
  const [x2, y2] = at(INNER, to);
  const [x3, y3] = at(INNER, from);
  return `M${x0} ${y0} A${OUTER} ${OUTER} 0 ${large} 1 ${x1} ${y1} ` +
    `L${x2} ${y2} A${INNER} ${INNER} 0 ${large} 0 ${x3} ${y3} Z`;
}

function el(name, attrs, text) {
  const node = document.createElementNS(SVG_NS, name);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
  if (text !== undefined) node.textContent = text;
  return node;
}

function render() {
  const { slices, caption } = content;
  const count = slices.length;
  const gap = 2 / OUTER;
  const mid = (OUTER + INNER) / 2;
  svg.replaceChildren();

  slices.forEach((slice, i) => {
    const a = angleOf(i, count);
    const half = Math.PI / count - gap / 2;
    const d = segmentPath(a - half, a + half);
    const hover = highlighted === i;
    svg.append(el("path", { d, class: "backdrop" }));
    svg.append(el("path", { d, class: hover ? "slice hover" : "slice" }));
    const [x, y] = at(mid, a);
    const cls = hover ? "label hover" : "label";
    svg.append(el("text", { x, y, class: cls }, slice.label));
  });

  svg.append(el("circle", { cx: C, cy: C, r: INNER - 6, class: "backdrop" }));
  const active = highlighted === null ? null : slices[highlighted];
  let lines = [caption];
  if (active) lines = [active.label];
  lines.forEach((line, i) => {
    const y = C + (i - (lines.length - 1) / 2) * 17;
    svg.append(el("text", { x: C, y, class: "caption" }, line));
  });
}

// Report once the frame after this render has been composited.
function reportPainted() {
  requestAnimationFrame(() =>
    requestAnimationFrame(() => window.__TAURI__.core.invoke("wheel_painted")));
}

window.__TAURI__.event.listen("wheel:content", (event) => {
  content = event.payload;
  highlighted = null;
  render();
  reportPainted();
});

window.__TAURI__.event.listen("wheel:highlight", (event) => {
  highlighted = event.payload;
  render();
});

render();
reportPainted();
