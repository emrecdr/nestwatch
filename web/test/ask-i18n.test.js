// The child's page speaks one language, completely.
//
// `assets/ask.js` swaps the markup from a table keyed by `data-i18n`. The failure this guards is
// not a crash: a key present in English and missing in Dutch leaves one English sentence sitting in
// an otherwise Dutch page, which reads as a half-finished product to the person it is addressed to
// — and the one sentence most likely to be missed is the longest, which is the disclosure.
//
// Read as text rather than executed. `ask.js` is a browser script that reaches for `document` at
// load, and standing up a DOM to check a data table would be a larger decision than the check is
// worth (the same reasoning harness.js records for app.js).

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const ASK_JS = readFileSync(join(here, "..", "..", "assets", "ask.js"), "utf8");
const ASK_HTML = readFileSync(join(here, "..", "..", "assets", "ask.html"), "utf8");

/** Keys defined in an object literal block, e.g. `nl: { … }` or `const EN = { … }`. */
function keysOf(block) {
  return new Set([...block.matchAll(/^\s{2,4}([A-Za-z][A-Za-z0-9]*):/gm)].map((m) => m[1]));
}

function blockAfter(marker) {
  const start = ASK_JS.indexOf(marker);
  assert.notEqual(start, -1, `could not find ${marker} in ask.js`);
  const open = ASK_JS.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < ASK_JS.length; i += 1) {
    if (ASK_JS[i] === "{") depth += 1;
    else if (ASK_JS[i] === "}") {
      depth -= 1;
      if (depth === 0) return ASK_JS.slice(open, i + 1);
    }
  }
  throw new Error(`unterminated block after ${marker}`);
}

test("every string the script builds in English has a Dutch counterpart", () => {
  const en = keysOf(blockAfter("const EN = "));
  const nl = keysOf(blockAfter("  nl: "));

  assert.ok(en.size > 10, `expected a real English table, found ${en.size} keys`);

  const missing = [...en].filter((k) => !nl.has(k));
  assert.deepEqual(
    missing,
    [],
    `Dutch is missing ${missing.length} string(s) the script builds; each would appear in ` +
      `English on an otherwise Dutch page: ${missing.join(", ")}`,
  );
});

test("every key the markup asks for is answered by the Dutch table", () => {
  const nl = keysOf(blockAfter("  nl: "));
  const wanted = [
    ...ASK_HTML.matchAll(/data-i18n(?:-placeholder|-label)?="([A-Za-z0-9]+)"/g),
  ].map((m) => m[1]);

  assert.ok(wanted.length > 10, `expected the markup to be marked up, found ${wanted.length}`);

  const missing = [...new Set(wanted)].filter((k) => !nl.has(k));
  assert.deepEqual(
    missing,
    [],
    `ask.html marks these for translation but the Dutch table has no entry, so they would stay ` +
      `English: ${missing.join(", ")}`,
  );
});

test("the disclosure is translated, since it is the sentence that has to be understood", () => {
  const nl = blockAfter("  nl: ");
  assert.match(
    nl,
    /disclosure:/,
    "the notice telling the child what is watched must not be the one string left in English",
  );
  assert.match(nl, /gele rand/, "the yellow-border sentence must survive translation");
});
