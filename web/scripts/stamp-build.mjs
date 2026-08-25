// Advance the compiled stylesheet's modification time after a successful build.
//
// # Why this exists
//
// `build.rs` warns when `assets/app.css` is older than the files Tailwind scans, so nobody ships
// against a stylesheet they forgot to rebuild. That check reads mtime, which it treats as "when was
// this last built".
//
// **Tailwind does not write the file when the output is byte-identical.** Measured, twice, rather
// than assumed: touching `index.html` in a way that changes no class names left `app.css` at
// mtime 1787666447 and 89,906 bytes across a full successful `npm run build`; adding one new class
// moved it to 1787667523 and 89,930 bytes. So Tailwind's mtime answers "did the output change",
// while `build.rs` is asking "did you rebuild" — different questions, and they diverge on every
// edit that does not affect the CSS. Editing prose, changing a directive that uses only existing
// classes, adjusting Rust that happens to sit beside the markup: all of them produce a build that
// succeeds and a warning that says it did not happen.
//
// A check that fires when nothing is wrong is worse than no check, because it trains the reader to
// ignore the one time it is right. This script closes that gap by making the build declare its own
// completion: after Tailwind succeeds, the output's mtime becomes "now", which is what `build.rs`
// was reading it as all along.
//
// # Why not a content hash
//
// A stamp file holding a hash of the inputs would also survive the case below, and was considered.
// It costs a second generated artifact, a format shared between this script and `build.rs`, and a
// third thing to keep in step — for a warning whose real safety net is a test. Not proportionate.
//
// # What this does not fix
//
// Anything that rewrites a source's mtime without changing its content — `git checkout` or a rebase
// landing an identical file — still makes the sources look newer than a correct stylesheet. That
// warning is a false alarm too, it is rare, and it clears itself on the next build.
//
// # The real gate is a test, not this
//
// `web::tests::every_class_in_the_markup_has_a_rule_in_the_shipped_css` compares the markup against
// the *compiled* stylesheet and fails, naming the class, when one is missing. That runs in CI on
// both Ubuntu and Windows and has no false positives. The warning this script repairs is early
// feedback on top of it, which is why repairing it is worth a small script and not a large one.
//
// # The order of the build chain is load-bearing
//
// `package.json` runs `strip-comments.mjs && tailwindcss && this`, joined with `&&` so a failure
// anywhere stops the chain and nothing is stamped. Both halves of that matter:
//
//  * **`strip-comments.mjs` must run first**, because it regenerates `web/.scan/` — the copies
//    Tailwind actually scans. Stamping a stylesheet compiled from a stale scan would assert
//    freshness that is not there.
//  * **This must run last**, after Tailwind has succeeded. Stamping first, or on a failed build,
//    turns the warning inside out: instead of a false alarm it becomes a false *silence*, which is
//    strictly worse — the whole point of the check is to speak up when the stylesheet is behind.
//
// `web::tests::the_css_build_chain_stamps_only_after_a_successful_compile` pins that order, because
// a comment does not survive someone reordering a one-line npm script.
//
// # Why a script and not `touch`
//
// `npm run build` runs on `windows-latest` in both `ci.yml` and `release.yml`. `touch` is not a
// command there, so a shell one-liner would break the release build on the only platform that
// ships. Node is already a hard requirement of this build.

import { utimesSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const CSS = join(here, "..", "..", "assets", "app.css");

// Fail loudly if the stylesheet is not there. Reaching this script means Tailwind exited zero, so a
// missing output is a broken build pretending to be a working one — exactly what `build.rs`'s other
// warning exists to catch, and it should not be reached by way of a silent no-op here.
try {
  statSync(CSS);
} catch {
  console.error(
    `stamp-build: ${CSS} does not exist, but the build reported success.\n` +
      "Something is wrong with the Tailwind step; not stamping.",
  );
  process.exit(1);
}

const now = new Date();
utimesSync(CSS, now, now);
