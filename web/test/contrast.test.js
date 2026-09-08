// Every colour pair the dashboard actually paints meets WCAG AA.
//
// # Why this exists
//
// `web/src/app.css` already carries one measured contrast decision, and a good one: `bg-error`
// against `bg-primary` was found at **1.22** in the dim theme, identified as a red-green confusion
// pair at near-identical luminance, and fixed with a texture rather than a colour so the cue
// survives colour blindness. That is one pair. Two themes ship — `light --default` and
// `dim --prefersdark` — and every other combination was unmeasured.
//
// This sweeps them, and it found one: `secondary-content` on `secondary` is **3.05:1** in the
// light theme, against AA's 4.5:1 for normal text. It is daisyUI's own palette rather than
// anything this project chose, and it reached exactly one control — the child's **Redeem** button
// on `/ask`, at default button size, on the page belonging to the person with the least power to
// work around it.
//
// # Why it checks used surfaces rather than the whole palette
//
// A theme defines nine roles; this product paints six of them. Asserting over all nine would fail
// on colours nobody renders, and the fix for a guard that reports problems you cannot see is to
// delete the guard. Scanning the markup for the roles in use means the coverage grows by itself
// the day somebody writes `btn-accent`, which a hand-maintained list would not.

import test from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { contrastRatio, luminance, oklchToLinearSrgb, themes } from "./contrast.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const asset = (name) => readFileSync(join(here, "..", "..", "assets", name), "utf8");

const hex = (str) =>
  "#" +
  oklchToLinearSrgb(str)
    .map((c) => {
      const e = c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
      return Math.round(Math.min(1, Math.max(0, e)) * 255)
        .toString(16)
        .padStart(2, "0");
    })
    .join("");

// ── The maths, pinned against values known independently of this code ──────────────────────────

test("the converter agrees with the two ratios that are true by definition", () => {
  // WCAG defines the ratio as (L1 + 0.05) / (L2 + 0.05) with luminance in [0, 1], so black on
  // white is exactly 21 and anything on itself is exactly 1. A converter that is subtly wrong
  // still gets these two right only if the whole pipeline is right at both ends of the range.
  assert.equal(contrastRatio("oklch(0% 0 0)", "oklch(100% 0 0)").toFixed(2), "21.00");
  assert.equal(contrastRatio("oklch(100% 0 0)", "oklch(100% 0 0)").toFixed(2), "1.00");
});

test("the converter round-trips a colour whose sRGB value is known", () => {
  // sRGB red is oklch(62.8% 0.2577 29.23) by construction, so this pins the chroma and hue path —
  // which the two greys above cannot, both having zero chroma.
  assert.equal(hex("oklch(62.8% .2577 29.23)"), "#ff0000");
  assert.equal(hex("oklch(100% 0 0)"), "#ffffff");
});

test("a colour this module cannot read reports nothing rather than a plausible number", () => {
  // Fail-closed: a token written as `#1a1a1a` or `var(--x)` must not silently become black.
  assert.equal(luminance("#1a1a1a"), null);
  assert.equal(contrastRatio("oklch(0% 0 0)", "var(--whatever)"), null);
});

// ── The policy ─────────────────────────────────────────────────────────────────────────────────

/** The daisyUI semantic roles this product actually paints, read from what ships. */
function rolesInUse() {
  const source = [asset("index.html"), asset("ask.html"), asset("app.js")].join("\n");
  const ROLE = /\b(?:btn|bg|alert|badge|text|progress)-(primary|secondary|accent|neutral|info|success|warning|error)\b/g;
  return new Set([...source.matchAll(ROLE)].map((m) => m[1]));
}

test("every surface the pages paint meets WCAG AA in both themes", () => {
  const blocks = themes(asset("app.css"));
  const used = rolesInUse();

  // A reader that stopped finding themes, or a scan that stopped finding roles, must not be able
  // to pass by checking nothing at all.
  assert.ok(blocks.length >= 2, `found ${blocks.length} theme blocks; the reader is broken`);
  assert.ok(used.size >= 4, `found ${used.size} roles in use; the markup scan is broken`);

  const failures = [];
  for (const { selector, colors } of blocks) {
    // base-100/base-content is every page's body text, so it is checked whether or not a role
    // happens to name it.
    const pairs = [["base-100", "base-content"], ...[...used].map((r) => [r, `${r}-content`])];
    for (const [bg, fg] of pairs) {
      if (!colors[bg] || !colors[fg]) continue;
      const ratio = contrastRatio(colors[bg], colors[fg]);
      assert.ok(ratio !== null, `${fg} on ${bg} could not be measured in ${selector}`);
      if (ratio < 4.5) {
        failures.push(
          `${fg} on ${bg} = ${ratio.toFixed(2)}:1 (${hex(colors[fg])} on ${hex(colors[bg])}) ` +
            `in ${selector.slice(0, 48)}`,
        );
      }
    }
  }

  assert.deepEqual(
    failures,
    [],
    `WCAG AA asks 4.5:1 for normal text. These pairs are painted by the shipped markup and do ` +
      `not reach it:\n  ${failures.join("\n  ")}\n\nEither stop using that surface, or override ` +
      `the token in web/src/app.css — and if you override it, say why there, the way .st-over does.`,
  );
});
