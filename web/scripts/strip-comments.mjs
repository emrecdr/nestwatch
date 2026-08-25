// Write comment-free copies of the served files for Tailwind to scan.
//
// # Why this exists
//
// Tailwind finds class names by scanning source files as raw text. It cannot distinguish a class
// name from English prose, and daisyUI names its components with ordinary words — `list`, `tab`,
// `step`, `range`, `join`, `mask`, `collapse`, `tooltip`. So an explanatory comment ships whatever
// component it happens to mention, in full, to every install.
//
// This is not hypothetical and not carelessness. It happened twice in one day to two different
// authors: a comment reading "Width steps up with the screen" shipped 2,408 bytes of a widget the
// product does not have, and a later pass added `tab`, `list` and `step` for another 1,544. Both
// were found by measuring the output, never by reading the diff. No amount of care makes English
// avoid a vocabulary that includes "list", and the next person to write a comment will not have
// read either incident.
//
// # Why stripping is safe here
//
// Over-stripping is the failure that would matter — remove a real class name and the element
// renders unstyled with nothing reporting it. Two things make that safe rather than a gamble:
//
//  * The strip is conservative. The scanner below tracks string and template literals so a `//`
//    inside a string is never treated as a comment, and it only ever removes text it is certain is
//    a comment. When in doubt it keeps.
//  * `web::tests::every_class_in_the_markup_has_a_rule_in_the_shipped_css` compares the markup
//    against the *compiled* stylesheet. If this script ever removed something real, that test goes
//    red rather than the interface going quietly wrong. The two compose: this reduces the input,
//    that proves the output still covers the markup.
//
// # What is deliberately NOT stripped
//
// String contents. `assets/app.js` builds class strings at runtime — `stBarClass` returns
// "bg-error" / "bg-primary" / "st-nodata", named nowhere else — and `build.rs` calls that out as
// the reason the `.js` glob exists at all. Removing string bodies would purge exactly those.

import { readdirSync, readFileSync, writeFileSync, mkdirSync, rmSync, watch } from "node:fs";
import { join, dirname, extname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const ASSETS = join(here, "..", "..", "assets");
const OUT = join(here, "..", ".scan");

/** Remove `<!-- … -->`. They cannot nest, so a non-greedy sweep is exact. */
function stripHtml(src) {
  return src.replace(/<!--[\s\S]*?-->/g, "");
}

/**
 * Remove `//` and block comments from JavaScript, leaving string and template contents intact.
 *
 * A hand-written scanner rather than a regex because the cases that matter are exactly the ones a
 * regex gets wrong: `"https://example"` is not a comment, and `` `a ${b} // c` `` is not either.
 * Regex literals are left alone by construction — a `/` is only treated as opening a comment when
 * the very next character is `/` or `*`, and `/\.exe$/i` starts `/\`.
 */
function stripJs(src) {
  let out = "";
  let i = 0;
  let quote = null; // the character that closes the string we are inside, or null

  while (i < src.length) {
    const c = src[i];
    const next = src[i + 1];

    if (quote) {
      out += c;
      if (c === "\\") {
        // Escaped character: copy it wholesale so a `\"` cannot end the string.
        out += src[i + 1] ?? "";
        i += 2;
        continue;
      }
      if (c === quote) quote = null;
      i += 1;
      continue;
    }

    if (c === '"' || c === "'" || c === "`") {
      quote = c;
      out += c;
      i += 1;
      continue;
    }

    if (c === "/" && next === "/") {
      while (i < src.length && src[i] !== "\n") i += 1;
      continue; // the newline itself is copied by the next iteration
    }

    if (c === "/" && next === "*") {
      i += 2;
      while (i < src.length && !(src[i] === "*" && src[i + 1] === "/")) i += 1;
      i += 2;
      continue;
    }

    out += c;
    i += 1;
  }
  return out;
}

/**
 * Whether this file is ours to strip.
 *
 * Vendored and minified code is excluded, and finding out why cost a measurement: pointed at
 * `alpine.min.js`, the scanner below removed **13,543 bytes** from a file that contains no comments
 * at all. Minified output is full of regex literals and divisions, and a `/` there is not a comment
 * marker — the scanner mangled the library into different tokens, which produced a *larger*
 * stylesheet rather than a smaller one.
 *
 * Excluding it is also just correct. The scan exists to find the classes *this project's markup*
 * uses; a third-party library defines none of them, and scanning it only adds noise that happens to
 * look like class names. That was true before this script existed — the old glob scanned it too.
 */
function ours(name) {
  return !name.endsWith(".min.js");
}

function build() {
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(OUT, { recursive: true });
  let n = 0;
  for (const name of readdirSync(ASSETS)) {
    const ext = extname(name);
    if ((ext !== ".html" && ext !== ".js") || !ours(name)) continue;
    const src = readFileSync(join(ASSETS, name), "utf8");
    const out = ext === ".html" ? stripHtml(src) : stripJs(src);
    // A file that lost most of itself was not understood. Comments are a minority of any source
    // here; anything past half means the scanner mis-tracked a string or a regex and is deleting
    // code. Failing the build is right — the alternative is a stylesheet quietly built from a
    // corrupted reading of the input.
    //
    // **How this was found, because the tell is counter-intuitive and worth keeping.** Pointed at
    // `alpine.min.js` this scanner removed 13,543 bytes from a file containing no comments. Nothing
    // in the Rust suite could see it: the served library was never touched (only copies under
    // `.scan/` are written), every class the markup uses still had a rule, and every test passed.
    // The only symptom was that **the stylesheet got bigger when it should have got smaller** —
    // mangled tokens produce *more* candidate class names, not fewer.
    //
    // So the diagnostic is: if a change meant to shrink the output grows it, something is being
    // mis-read rather than merely under-optimised. A silent deletion of code shows up as an
    // *increase* downstream, which is the opposite of where anyone looks first.
    if (src.length > 200 && out.length < src.length * 0.5) {
      throw new Error(
        `strip-comments: ${name} lost ${src.length - out.length} of ${src.length} bytes. ` +
          `That is not comments — the scanner mis-parsed it. Refusing to build from it.`,
      );
    }
    writeFileSync(join(OUT, name), out);
    n += 1;
  }
  return n;
}

const n = build();
if (process.argv.includes("--watch")) {
  // Tailwind watches what `@source` points at, which is now the stripped copies — so an edit to a
  // served file has to reach them or the dev loop silently stops updating.
  console.log(`scanning ${n} files, watching ${ASSETS}`);
  let queued = null;
  watch(ASSETS, () => {
    clearTimeout(queued);
    queued = setTimeout(build, 50); // coalesce the burst an editor's save produces
  });
} else {
  console.log(`scanning ${n} files`);
}
