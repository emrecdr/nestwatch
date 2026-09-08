// Colour maths for the contrast guard: enough of OKLCH → sRGB to compute a WCAG contrast ratio.
//
// # Why this exists rather than a dependency
//
// daisyUI 5 writes every theme colour as `oklch(65% .241 354.308)`, and WCAG contrast is defined on
// sRGB relative luminance. Converting between them is about forty lines of published, deterministic
// matrix arithmetic, so a package would be a supply-chain entry — audited by `cargo deny`'s
// counterpart on the Node side, which does not exist — for arithmetic that cannot drift.
//
// # Why it is its own module with its own tests
//
// A converter that is quietly wrong produces a *plausible* number, and a guard built on it would
// then police the wrong thing while looking like it worked. That is the failure this repository
// spends the most effort on. So the maths is separated from the policy, and
// `contrast.test.js` pins it against values that are known independently of this code: black on
// white is exactly 21:1 by definition, and sRGB red is `oklch(62.8% .2577 29.23)` by construction.
//
// Sources: Björn Ottosson's OKLab definition for the matrices, WCAG 2.2 for the ratio and the
// 4.5:1 threshold.

/** `oklch(L% C H)` → linear-light sRGB. Returns null for anything that is not an oklch() colour. */
export function oklchToLinearSrgb(str) {
  const m = str.match(/oklch\(\s*([\d.]+)%?\s+([\d.]+)\s+([\d.]+)/);
  if (!m) return null;
  const L = parseFloat(m[1]) / (str.includes("%") ? 100 : 1);
  const C = parseFloat(m[2]);
  const H = (parseFloat(m[3]) * Math.PI) / 180;

  const a = C * Math.cos(H);
  const b = C * Math.sin(H);

  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.2914855480 * b;

  const l = l_ ** 3;
  const mm = m_ ** 3;
  const s = s_ ** 3;

  return [
    4.0767416621 * l - 3.3077115913 * mm + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * mm - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * mm + 1.7076147010 * s,
  ];
}

const encode = (c) => (c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055);
const decode = (c) => (c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4));

/**
 * WCAG relative luminance of an `oklch()` colour.
 *
 * Clamped through the sRGB encoding rather than in linear light, because that is what a display
 * does with an out-of-gamut colour: the browser encodes, the panel clips. Clamping the linear
 * values instead gives a different — and wrong — answer for saturated colours, which is exactly
 * the region every accent in a theme lives in.
 */
export function luminance(str) {
  const linear = oklchToLinearSrgb(str);
  if (linear === null) return null;
  const [r, g, b] = linear.map((c) => decode(Math.min(1, Math.max(0, encode(c)))));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG contrast ratio between two `oklch()` colours, or null if either cannot be read. */
export function contrastRatio(a, b) {
  const [x, y] = [luminance(a), luminance(b)];
  if (x === null || y === null) return null;
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/**
 * Every daisyUI theme block in the compiled stylesheet, as `{ selector, colors }`.
 *
 * A theme is recognised by carrying `--color-base-100`, rather than by matching a selector: the
 * selectors daisyUI emits are long, and they change between releases (`:where(:root)`,
 * `[data-theme=dim]`, a `:has()` on a theme-controller input). Keying on the token means a renamed
 * selector is invisible here instead of silently reducing the guard to nothing.
 */
export function themes(css) {
  const out = [];
  for (const m of css.matchAll(/\{([^{}]*--color-base-100:[^{}]*)\}/g)) {
    const colors = {};
    for (const c of m[1].matchAll(/--color-([a-z0-9-]+):\s*(oklch\([^)]*\))/g)) colors[c[1]] = c[2];
    const start = css.lastIndexOf("}", m.index) + 1;
    out.push({ selector: css.slice(start, m.index).trim(), colors });
  }
  return out;
}
