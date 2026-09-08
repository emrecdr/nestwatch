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

import { contrastRatio, hex, luminance, themes } from "./contrast.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const asset = (name) => readFileSync(join(here, "..", "..", "assets", name), "utf8");

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

/**
 * The daisyUI semantic roles this product actually paints, read from what ships.
 *
 * Both halves are derived. The *vocabulary* comes from the theme itself — every token for which
 * the stylesheet defines both a surface and a matching `-content` — rather than being listed
 * here, and the scan matches any utility ending in one of those roles rather than an enumerated
 * set of prefixes. The enumerated version missed `toggle-primary`, `toggle-warning` and
 * `border-warning`, all painted by the shipped markup: it was only ever green because those three
 * roles happened to be reachable through a prefix that was on the list. That is precisely how
 * `secondary` — the one role that failed — was reachable only through `btn-secondary`.
 */
function rolesInUse(colors) {
  const roles = Object.keys(colors).filter((k) => !k.endsWith("-content") && colors[`${k}-content`]);
  const source = [asset("index.html"), asset("ask.html"), asset("app.js")].join("\n");
  const ROLE = new RegExp(`\\b[a-z]+(?:-[a-z]+)*?-(${roles.join("|")})\\b`, "g");
  return new Set([...source.matchAll(ROLE)].map((m) => m[1]));
}

test("every surface the pages paint meets WCAG AA in both themes", () => {
  const blocks = themes(asset("app.css"));
  const used = rolesInUse(blocks[0].colors);

  // A reader that stopped finding themes, or a scan that stopped finding roles, must not be able
  // to pass by checking nothing at all.
  assert.ok(blocks.length >= 2, `found ${blocks.length} theme blocks; the reader is broken`);
  assert.ok(used.size >= 4, `found ${used.size} roles in use; the markup scan is broken`);

  const failures = [];
  for (const { selector, colors } of blocks) {
    // Every base tier the theme defines, against base-content — that is the page's own body text,
    // so it is checked whether or not a role happens to name it. All of `base-100`, `base-200` and
    // `base-300` are painted by the markup; pinning only `base-100` left the cards and the drawer
    // that use the other two unmeasured.
    const tiers = Object.keys(colors).filter((k) => /^base-\d+$/.test(k));
    const pairs = [
      ...tiers.map((t) => [t, "base-content"]),
      ...[...used].map((r) => [r, `${r}-content`]),
    ];
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
