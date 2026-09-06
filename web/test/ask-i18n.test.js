// The child's page speaks one language, completely.
//
// `assets/ask.js` swaps the markup from a table keyed by `data-i18n`. The failure this guards is
// not a crash: a key present in English and missing in another table leaves one English sentence
// sitting in an otherwise translated page, which reads as a half-finished product to the person it
// is addressed to — and the one sentence most likely to be missed is the longest, which is the
// disclosure.
//
// **Every table is checked, discovered rather than named.** This file used to say `nl` in eight
// places. That is the same tautological-fixture trap `config::Language::ALL` exists to close on the
// Rust side: a hardcoded list keeps passing while a third language goes completely unread, and
// Turkish was added on exactly the day that would have happened. The tables are found by scanning
// `STRINGS` for its keys, so a fourth language is covered by existing here rather than by anyone
// remembering to extend a list.
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

/**
 * Every translated table in `STRINGS`, as `[tag, block]`.
 *
 * `en` is deliberately absent: it is `null` in the source because the markup is already English,
 * so there is no table to compare and nothing it could be missing.
 */
function translatedTables() {
  const tags = [...ASK_JS.matchAll(/^ {2}([a-z]{2}): \{/gm)].map((m) => m[1]);
  // A broken scan must not pass by finding nothing to check.
  assert.ok(
    tags.length >= 2,
    `expected at least two translated tables in STRINGS, found ${tags.length} (${tags}) — the ` +
      `scanner is broken, not the translations`,
  );
  return tags.map((tag) => [tag, blockAfter(`  ${tag}: {`)]);
}

test("every string the script builds in English has a counterpart in each language", () => {
  const en = keysOf(blockAfter("const EN = "));
  assert.ok(en.size > 10, `expected a real English table, found ${en.size} keys`);

  for (const [tag, block] of translatedTables()) {
    const missing = [...en].filter((k) => !keysOf(block).has(k));
    assert.deepEqual(
      missing,
      [],
      `${tag} is missing ${missing.length} string(s) the script builds; each would appear in ` +
        `English on an otherwise ${tag} page: ${missing.join(", ")}`,
    );
  }
});

test("every key the markup asks for is answered by each language", () => {
  const wanted = [
    ...ASK_HTML.matchAll(/data-i18n(?:-placeholder|-label)?="([A-Za-z0-9]+)"/g),
  ].map((m) => m[1]);
  assert.ok(wanted.length > 10, `expected the markup to be marked up, found ${wanted.length}`);

  for (const [tag, block] of translatedTables()) {
    const keys = keysOf(block);
    const missing = [...new Set(wanted)].filter((k) => !keys.has(k));
    assert.deepEqual(
      missing,
      [],
      `ask.html marks these for translation but the ${tag} table has no entry, so they would ` +
        `stay English: ${missing.join(", ")}`,
    );
  }
});

test("the disclosure survives into every language, whole", () => {
  for (const [tag, block] of translatedTables()) {
    assert.match(
      block,
      /disclosure:/,
      `the notice telling the child what is watched must not be the one string left in English (${tag})`,
    );
    // "Windows" is a proper noun and stays untranslated, which makes it a language-independent
    // proxy for the sentence about the yellow border still being there. Checking for a translated
    // keyword instead would mean a per-language word list — the thing this file just stopped
    // doing. A disclosure that lost its second half fails here rather than shipping short.
    const disclosure = block.slice(block.indexOf("disclosure:"));
    assert.ok(
      disclosure.includes("Windows"),
      `the ${tag} disclosure no longer mentions Windows, so the yellow-border sentence — the one ` +
        `thing on this page that tells the child when they are being watched — has been dropped`,
    );
  }
});
