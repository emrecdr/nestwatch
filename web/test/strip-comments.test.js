import test from "node:test";
import assert from "node:assert/strict";

import { stripJs } from "../scripts/strip-comments.mjs";

// --- the build guard's trigger ---------------------------------------------
//
// The guard used to fire on a ratio: losing more than half the bytes failed the build, justified as
// "comments are a minority of any source here". They are not — `assets/app.js` sits at ~49.6%, and
// the house style is deliberately heavy explanatory prose, so the guard punished the one thing this
// codebase most wants people to do. Worse, its message ("the scanner mis-parsed it") was false
// whenever it fired for that reason.
//
// A scan that ends still inside a string is a *definite* mis-parse: it ran off the end of the input
// looking for a closing quote. That is the property worth failing on.

test("a scan that runs off the end of a string is reported as a mis-parse", () => {
  const r = stripJs('const a = "unterminated;\nconst b = 1;\n');
  assert.equal(r.unterminated, true, "no closing quote — the scanner is lost");
});

test("comment-heavy source is not a mis-parse, however many bytes it loses", () => {
  const src = "// " + "x".repeat(4000) + "\nconst a = 1;\n";
  const r = stripJs(src);
  assert.equal(r.unterminated, false, "losing most of a file to comments is normal here");
  assert.ok(r.text.includes("const a = 1;"), "the code itself survives");
});

test("a // inside a string is still not a comment", () => {
  const r = stripJs('const u = "https://example.com/x"; // gone\n');
  assert.ok(r.text.includes("https://example.com/x"), "URLs are not comments");
  assert.ok(!r.text.includes("gone"), "the real comment still goes");
  assert.equal(r.unterminated, false);
});

test("importing the stripper does not run a build", () => {
  assert.equal(typeof stripJs, "function", "the module must be importable for its own tests");
});
