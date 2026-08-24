// Load `assets/app.js` in Node so its pure methods can be tested.
//
// The file is a classic browser script, not a module: it declares `function app()` at the top
// level and expects a browser to make that a global. Rather than restructure it for the test's
// convenience — which would mean the tests exercise a shape the browser never loads — it is
// evaluated here the same way a `<script src>` tag evaluates it, in a `vm` context whose globals
// are stubs.
//
// Deliberately NOT a DOM library. Only the methods that touch no browser API are tested (see
// `pureApp`), which is the same split the Rust side already uses: a pure decision layer that can
// be tested cheaply, and an I/O layer that cannot. Adding jsdom to reach the rest would be a
// second, larger decision than "there should be some JavaScript tests at all".

import { readFileSync } from "node:fs";
import { createContext, runInContext } from "node:vm";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
export const APP_JS = join(here, "..", "..", "assets", "app.js");

/**
 * Evaluate app.js and return its `app()` component object.
 *
 * The globals below are the ones the *constructor* touches while building the object. A method
 * that reaches for anything else throws a ReferenceError in the test rather than silently
 * returning undefined, which is the intent: it marks that method as needing a browser, not as
 * passing.
 */
export function loadApp(globals = {}) {
  const source = readFileSync(APP_JS, "utf8");
  const sandbox = {
    // A method that hits the network in a test is a bug in the test, so make it loud rather
    // than letting it hang or silently resolve. Override via `globals` to test a method that
    // legitimately fetches — see the resolveTimeRequest tests, which record the URL it chose.
    fetch() {
      throw new Error("fetch() called: this method is not pure and must not be tested here");
    },
    ...globals,
  };
  // The script runs in its own realm, so assigning `globalThis.fetch` from the test's realm does
  // not reach it — anything the code under test should see has to be in this object.
  sandbox.globalThis = sandbox;
  const context = createContext(sandbox);
  // `; app` makes the declared function the completion value, the same way the browser would
  // expose it on window.
  return runInContext(`${source}\n;app`, context, { filename: APP_JS })();
}

/**
 * The component with `state` merged in, for methods that read `this`.
 */
export function withState(state) {
  return Object.assign(loadApp(), state);
}
