// Mutation testing for the dashboard's JavaScript (`assets/app.js`), the JavaScript half of what
// `cargo-mutants` does for the Rust (`docs/OPEN-FINDINGS.md` `O75`).
//
// Run from the repository root — `npm run mutate` does that — because the sandbox Stryker copies
// has to hold both the code it mutates (`assets/`) and the tests that judge it (`web/test/`). On
// its own it mutates the whole file, which is minutes; CI passes `--mutate` the line ranges a push
// changed, which is seconds (`ci.yml`, the `web-mutants` job).
//
// Two things this suite needs that a stock Stryker setup does not, both found by running it:
//
// - The tests evaluate `app.js` in a `vm` context, and Stryker's mutant switch reads the live
//   mutant from `globalThis.process.env`, which that context lacks. `web/test/harness.js` passes
//   it in; without that every mutant reports as surviving.
// - Some tests read `app.js` as text, and a mutation run rewrites that text. They are marked
//   `READS_SOURCE` and skip when `NESTWATCH_MUTATION_RUN` is set, which only the command below
//   does; the ordinary `npm test` runs them in full.
//
// `ask.js` is not mutated: its tests read its text and none runs it, so every mutant would survive
// whatever it changed. That gap is `O75`'s remaining half.
//
// `web/package.json` overrides two packages inside Stryker's own tree, because Stryker pins them
// below their security fixes: `minimatch` (pinned `~10.2.4`, fixed in 11.0.1) and `qs` (pinned
// exactly 6.15.1 by `typed-rest-client`, fixed in 6.16.1). Neither is reachable the way this runs —
// `qs` serves only the dashboard reporter, which is not used, and `minimatch` sees only the
// patterns below — but an open advisory is an open advisory. Checked with minimatch 11 against a
// whole-file dry run, a run that must fail on survivors, and a range with nothing to mutate. Drop
// them once Stryker raises its pins.
export default {
  testRunner: "command",
  commandRunner: {
    command: 'cd web && NESTWATCH_MUTATION_RUN=1 node --test "test/*.test.js"',
  },
  mutate: ["assets/app.js"],
  // Only what the tests read goes into the sandbox, not the Rust tree beside it.
  ignorePatterns: ["/*", "!/assets/", "!/web/", "/web/node_modules/", "/web/.scan/"],
  // The command runner cannot report which tests cover a mutant, so every mutant runs the suite.
  // It takes about half a second.
  coverageAnalysis: "off",
  reporters: ["clear-text", "progress"],
  tempDirName: ".stryker-tmp",
  // Any survivor fails the run. As with the Rust job, a mutant that is genuinely equivalent is
  // marked where it lives (`// Stryker disable next-line <mutator>: <why>`), not tolerated here.
  thresholds: { high: 100, low: 100, break: 100 },
};
