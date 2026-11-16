// Run `assets/ask.js` against a stub page, so what the child's page shows and sends can be tested.
//
// `ask.js` is not a component with methods to call, the way `app.js` is: it runs from the top,
// looks up its elements by id, fetches `/status` at once and wires two forms. Everything it does
// lands in the page or goes over `fetch`, so the only way to test it is to give it a page and a
// network and read both back. Until this existed its tests read it as text and nothing ran it, so a
// silent revert there was found only by somebody looking; the `web-mutants` job in `ci.yml` now
// finds it.
//
// Hand-written rather than a DOM library, for the reason `harness.js` gives for `app.js`: the stub
// carries exactly the surface this script touches. The elements are the ones `ask.html` declares,
// by id and by `data-i18n`, read from the real markup, so a renamed id fails here rather than
// passing against a stub that still has the old one. A browser API the script starts to use and
// this stub lacks is a TypeError in the test, which is the intent.

import { readFileSync } from "node:fs";
import { createContext, runInContext } from "node:vm";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
export const ASK_JS = join(here, "..", "..", "assets", "ask.js");
const ASK_HTML = join(here, "..", "..", "assets", "ask.html");

/** One element, with the properties `ask.js` writes recorded so a test can read them back. */
export class StubElement {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.className = "";
    this.attributes = {};
    this.dataset = {};
    this.style = {};
    this.listeners = {};
    this.disabled = false;
    this.value = "";
    this.placeholder = "";
    this.resets = 0;
    this.ownText = "";
  }
  // Setting the text replaces the children, as it does in a browser — `renderWeek` clears the week
  // that way before drawing it again.
  get textContent() {
    return this.ownText + this.children.map((c) => c.textContent).join("");
  }
  set textContent(v) {
    this.children = [];
    this.ownText = String(v);
  }
  // The script only ever clears with this; anything else would need a parser this stub has not.
  set innerHTML(v) {
    if (v !== "") throw new Error(`ask.js set innerHTML to ${JSON.stringify(v)}`);
    this.textContent = "";
  }
  append(...nodes) {
    this.children.push(...nodes);
  }
  setAttribute(name, value) {
    this.attributes[name] = String(value);
  }
  getAttribute(name) {
    return this.attributes[name] ?? null;
  }
  addEventListener(type, handler) {
    this.listeners[type] = handler;
  }
  reset() {
    this.resets += 1;
  }
}

/** The elements `ask.html` declares: those with an `id`, and those carrying a `data-i18n*` key. */
function pageFromMarkup() {
  const html = readFileSync(ASK_HTML, "utf8");
  const byId = new Map();
  const translated = [];
  for (const [tag] of html.matchAll(/<[a-z][a-z0-9]*\b[^>]*>/g)) {
    const name = tag.match(/^<([a-z][a-z0-9]*)/)[1];
    const id = tag.match(/\sid="([^"]+)"/)?.[1];
    const keys = {
      i18n: tag.match(/\sdata-i18n="([^"]+)"/)?.[1],
      i18nPlaceholder: tag.match(/\sdata-i18n-placeholder="([^"]+)"/)?.[1],
      i18nLabel: tag.match(/\sdata-i18n-label="([^"]+)"/)?.[1],
    };
    if (!id && !keys.i18n && !keys.i18nPlaceholder && !keys.i18nLabel) continue;
    const el = new StubElement(name);
    for (const [k, v] of Object.entries(keys)) if (v) el.dataset[k] = v;
    if (id) byId.set(id, el);
    if (keys.i18n || keys.i18nPlaceholder || keys.i18nLabel) translated.push(el);
  }
  return { byId, translated };
}

/** A `fetch` answer: `{status, body}` becomes a response; an `Error` is a failed request. */
function respond(answer) {
  if (answer instanceof Error) throw answer;
  const status = answer.status ?? 200;
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => {
      if (answer.body === undefined) throw new SyntaxError("no JSON body");
      return answer.body;
    },
  };
}

/**
 * Load the child's page.
 *
 * `routes` maps a path to what it answers: a `{status, body}` object, an `Error` for a request that
 * never arrived, or a function of the request returning either. The page fetches `/status` as it
 * loads, so call `settle()` before reading what it drew.
 */
export function loadAsk(routes = {}) {
  const { byId, translated } = pageFromMarkup();
  const fetches = [];
  let poll = null;
  const document = {
    hidden: false,
    documentElement: { lang: "en" },
    getElementById: (id) => byId.get(id) ?? null,
    querySelectorAll(selector) {
      const key = {
        "[data-i18n]": "i18n",
        "[data-i18n-placeholder]": "i18nPlaceholder",
        "[data-i18n-label]": "i18nLabel",
      }[selector];
      if (!key) throw new Error(`the stub page answers no selector ${selector}`);
      return translated.filter((el) => el.dataset[key] !== undefined);
    },
    // A browser throws on a name that is not a tag, and so does this — an empty one renders a
    // broken page there, and must not pass here.
    createElement(tag) {
      if (!/^[a-z][a-z0-9-]*$/.test(tag)) throw new Error(`InvalidCharacterError: <${tag}>`);
      return new StubElement(tag);
    },
  };
  const sandbox = {
    document,
    async fetch(url, init = {}) {
      fetches.push({
        url,
        method: init.method ?? "GET",
        type: init.headers?.["Content-Type"],
        body: init.body ? JSON.parse(init.body) : undefined,
      });
      let answer = routes[url];
      if (answer === undefined) throw new Error(`the stub network has no route for ${url}`);
      if (typeof answer === "function") answer = answer(init);
      return respond(answer);
    },
    setInterval(fn) {
      poll = fn;
      return 1;
    },
    // See `harness.js`: a mutation run names the live mutant in this one variable, and Stryker's
    // switch reads it from `globalThis.process.env`, which this context does not otherwise have.
    ...(process.env.__STRYKER_ACTIVE_MUTANT__ === undefined
      ? {}
      : { process: { env: { __STRYKER_ACTIVE_MUTANT__: process.env.__STRYKER_ACTIVE_MUTANT__ } } }),
  };
  sandbox.globalThis = sandbox;
  runInContext(readFileSync(ASK_JS, "utf8"), createContext(sandbox), { filename: ASK_JS });

  return {
    el: (id) => byId.get(id),
    translated,
    document,
    fetches,
    routes,
    /** Run the minute poll once, the way the page's own interval would. */
    poll: () => poll(),
    /** Let every pending request and its handler finish. */
    settle: async () => {
      for (let i = 0; i < 10; i += 1) await new Promise((resolve) => setImmediate(resolve));
    },
    /** Submit a form as the browser would, and wait for its handler to finish. */
    async submit(id) {
      const event = { prevented: false, preventDefault() { this.prevented = true; } };
      await byId.get(id).listeners.submit(event);
      return event;
    },
  };
}
