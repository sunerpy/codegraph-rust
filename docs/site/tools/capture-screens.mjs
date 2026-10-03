#!/usr/bin/env node
/*
 * The website's screenshots of the browser viewer: docs/site/public/screens/<name>-<theme>.webp.
 *
 * The viewer must already be serving the screenshot corpus. `capture-screens.sh` next to this
 * file builds that corpus, starts the viewer and runs this script; docs/site/README.md explains
 * the procedure.
 *
 *   node docs/site/tools/capture-screens.mjs http://127.0.0.1:4791 [out-dir]
 *
 * Every capture is 1440 × 900 at device pixel ratio 1, saved as WebP at quality 85, once in the
 * light theme and once in the dark one. Each one is a fresh document load, and the script waits
 * for that document's load event, for every request to finish (the live /api/events stream
 * aside), for no loading marker to remain and for 600 ms without a DOM change. It then checks the
 * address and a piece of text the view must show. It stops, writing nothing further, on a
 * timeout, a wrong address, a missing text, a console error, an uncaught exception, a failed or
 * non-2xx request, or a symbol lookup that does not find exactly one match.
 *
 * Only Chrome's DevTools protocol is used, over Node's built-in WebSocket; nothing is installed.
 * CODEGRAPH_CHROME names the Chrome or Chromium executable.
 */
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const [base, outArg] = process.argv.slice(2);
if (!base) {
  console.error("usage: node capture-screens.mjs <viewer-url> [out-dir]");
  process.exit(2);
}
const CHROME = process.env.CODEGRAPH_CHROME;
if (!CHROME || !existsSync(CHROME)) {
  console.error("set CODEGRAPH_CHROME to a Chrome or Chromium executable");
  process.exit(2);
}
const outDir = resolve(outArg ?? join(here, "../public/screens"));
mkdirSync(outDir, { recursive: true });
const PORT = Number(process.env.CDP_PORT || 19631);
const WIDTH = 1440;
const HEIGHT = 900;
const THEMES = ["light", "dark"];

/** Symbols looked up by name, kind and file, so a capture never depends on a hard-coded id. */
const SYMBOLS = {
  resolve: {
    query: "IndexPaths::resolve",
    qualifiedName: "IndexPaths::resolve",
    kind: "method",
    file: "crates/codegraph-core/src/index_paths.rs",
  },
  frameworkResolver: {
    query: "FrameworkResolver",
    qualifiedName: "FrameworkResolver",
    kind: "trait",
    file: "crates/codegraph-resolve/src/framework.rs",
  },
};

/** name, route (after `#`), text the view must show. The list is exhaustive. */
const CAPTURES = [
  ["viewer-home", () => "/", "Index composition"],
  ["viewer-symbol", (ids) => `/s/${ids.resolve}`, "Called by"],
  ["viewer-hierarchy", (ids) => `/s/${ids.frameworkResolver}`, "Type hierarchy"],
  ["viewer-file", () => "/file/crates/codegraph-core/src/index_paths.rs", "Outline"],
  ["viewer-flow", () => "/flow?from=cmd_explore&to=explore_file_header", "Every hop"],
  ["viewer-map", () => "/map?root=crates&depth=1", "Architecture map"],
  ["viewer-dead", () => "/dead", "What the list leaves out"],
];

async function lookup({ query, qualifiedName, kind, file }) {
  const response = await fetch(`${base}/api/search?q=${encodeURIComponent(query)}&limit=50`);
  if (!response.ok) throw new Error(`search for ${query} answered ${response.status}`);
  const body = await response.json();
  const matches = (body.groups ?? [])
    .flatMap((group) => group.items)
    .filter(
      (item) => item.qualifiedName === qualifiedName && item.kind === kind && item.file === file,
    );
  if (matches.length !== 1) {
    throw new Error(
      `${kind} ${qualifiedName} in ${file}: expected one match, found ${matches.length}`,
    );
  }
  return matches[0].id;
}

const pause = (ms) => new Promise((done) => setTimeout(done, ms));

const chrome = spawn(
  CHROME,
  [
    "--headless=new",
    "--no-sandbox",
    "--disable-dev-shm-usage",
    "--hide-scrollbars",
    `--remote-debugging-port=${PORT}`,
    "about:blank",
  ],
  { stdio: ["ignore", "ignore", "pipe"] },
);

let failed = false;
try {
  let wsUrl;
  for (let i = 0; i < 100 && !wsUrl; i++) {
    try {
      wsUrl = (await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json())
        .webSocketDebuggerUrl;
    } catch {
      await pause(150);
    }
  }
  if (!wsUrl) throw new Error("Chrome's DevTools endpoint did not come up");

  const ws = new WebSocket(wsUrl);
  await new Promise((done, fail) => {
    ws.onopen = done;
    ws.onerror = fail;
  });
  let nextId = 0;
  const pending = new Map();
  const events = [];
  ws.onmessage = (message) => {
    const data = JSON.parse(message.data);
    if (data.id && pending.has(data.id)) {
      const { ok, fail } = pending.get(data.id);
      pending.delete(data.id);
      if (data.error) fail(new Error(JSON.stringify(data.error)));
      else ok(data.result);
    } else if (data.method) {
      events.push(data);
    }
  };
  const send = (method, params = {}, sessionId) =>
    new Promise((ok, fail) => {
      const id = ++nextId;
      pending.set(id, { ok, fail });
      ws.send(JSON.stringify({ id, method, params, sessionId }));
    });
  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  const cmd = (method, params) => send(method, params, sessionId);
  await cmd("Page.enable");
  await cmd("Runtime.enable");
  await cmd("Network.enable");
  await cmd("Emulation.setDeviceMetricsOverride", {
    width: WIDTH,
    height: HEIGHT,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await cmd("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  await cmd("Page.addScriptToEvaluateOnNewDocument", {
    source: `;(() => { window.__lastMutation = performance.now();
      new MutationObserver(() => { window.__lastMutation = performance.now(); })
        .observe(document, { subtree: true, childList: true, attributes: true, characterData: true }); })();`,
  });

  const evaluate = async (expression) => {
    const result = await cmd("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (result.exceptionDetails) {
      throw new Error(
        result.exceptionDetails.exception?.description ?? result.exceptionDetails.text,
      );
    }
    return result.result?.value;
  };

  /** Requests of the current document: id → url; the live event stream never finishes. */
  const inFlight = new Map();
  const problems = [];
  let seen = 0;
  function drain() {
    for (; seen < events.length; seen++) {
      const { method, params } = events[seen];
      if (method === "Network.requestWillBeSent" && !params.request.url.includes("/api/events")) {
        inFlight.set(params.requestId, params.request.url);
      } else if (method === "Network.responseReceived" && inFlight.has(params.requestId)) {
        const { status, url } = params.response;
        if (status >= 400) problems.push(`HTTP ${status} for ${url}`);
      } else if (method === "Network.loadingFinished") {
        inFlight.delete(params.requestId);
      } else if (method === "Network.loadingFailed") {
        if (inFlight.has(params.requestId))
          problems.push(`request failed: ${inFlight.get(params.requestId)} (${params.errorText})`);
        inFlight.delete(params.requestId);
      } else if (method === "Runtime.exceptionThrown") {
        problems.push(
          `uncaught exception: ${params.exceptionDetails.exception?.description ?? params.exceptionDetails.text}`,
        );
      } else if (method === "Runtime.consoleAPICalled" && params.type === "error") {
        problems.push(
          `console error: ${params.args.map((arg) => arg.value ?? arg.description).join(" ")}`,
        );
      }
    }
  }

  async function load(route) {
    await cmd("Page.navigate", { url: "about:blank" });
    drain();
    inFlight.clear();
    const mark = events.length;
    await cmd("Page.navigate", { url: `${base}/#${route}` });
    const started = Date.now();
    while (!events.slice(mark).some((event) => event.method === "Page.loadEventFired")) {
      if (Date.now() - started > 20000) throw new Error(`#${route}: the page did not load`);
      await pause(50);
    }
    while (Date.now() - started < 20000) {
      drain();
      const quiet =
        inFlight.size === 0 &&
        (await evaluate(`document.readyState === 'complete'
          && !document.querySelector('[aria-busy="true"], .skeleton, .loadingrow')
          && !document.body.innerText.includes('Reading the ')
          && performance.now() - (window.__lastMutation ?? 0) > 600`));
      if (quiet) return;
      await pause(100);
    }
    throw new Error(`#${route}: the view did not settle within 20 s`);
  }

  const ids = {};
  for (const [key, symbol] of Object.entries(SYMBOLS)) ids[key] = await lookup(symbol);

  // localStorage belongs to the viewer's origin, so the theme is set from one of its pages.
  await load("/");
  for (const theme of THEMES) {
    await evaluate(`localStorage.setItem('codegraph-ui.theme', '${theme}'); true`);
    for (const [name, route, expected] of CAPTURES) {
      const hash = route(ids);
      await load(hash);
      drain();
      if (problems.length > 0) throw new Error(`${name}-${theme}: ${problems.join("; ")}`);
      if ((await evaluate("location.hash")) !== `#${hash}`)
        throw new Error(`${name}-${theme}: the address moved away from #${hash}`);
      if (!(await evaluate(`document.body.innerText.includes(${JSON.stringify(expected)})`))) {
        throw new Error(`${name}-${theme}: the view does not show "${expected}"`);
      }
      if ((await evaluate("document.documentElement.dataset.theme")) !== theme) {
        throw new Error(`${name}-${theme}: the page is not in the ${theme} theme`);
      }
      const shot = await cmd("Page.captureScreenshot", { format: "webp", quality: 85 });
      const file = join(outDir, `${name}-${theme}.webp`);
      writeFileSync(file, Buffer.from(shot.data, "base64"));
      console.log(file);
    }
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  failed = true;
} finally {
  chrome.kill("SIGKILL");
}
process.exit(failed ? 1 : 0);
