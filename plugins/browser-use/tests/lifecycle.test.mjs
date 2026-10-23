import assert from "node:assert/strict";
import { EventEmitter, once } from "node:events";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { BrowserSession } from "../browser.mjs";

// These lifecycle regressions never import Playwright or launch a browser.
const deferred = () => Promise.withResolvers();
const tick = () => new Promise((resolve) => setImmediate(resolve));
const context = () => ({
  setDefaultTimeout() {},
  setDefaultNavigationTimeout() {},
  async addInitScript() {},
  on() {},
});
function browser(options = {}) {
  const instance = new EventEmitter();
  instance.closeCalls = 0;
  instance.isConnected = () => instance.closeCalls === 0;
  instance.newContext = options.newContext ?? (async () => context());
  instance.close = async () => {
    instance.closeCalls += 1;
    await options.close?.();
    instance.emit("disconnected");
  };
  return instance;
}

test("close waits for a pending launch and closes the late browser", async () => {
  const launch = deferred();
  const finishClose = deferred();
  const launched = browser({ close: () => finishClose.promise });
  const session = new BrowserSession({
    chromium: { launch: () => launch.promise },
  });
  const opening = assert.rejects(session.ensureBrowser(), {
    code: "session_cancelled",
  });
  let closed = false;
  const closing = session.close().then(() => {
    closed = true;
  });
  await tick();
  assert.equal(closed, false);
  launch.resolve(launched);
  await tick();
  assert.equal(launched.closeCalls, 1);
  assert.equal(
    closed,
    false,
    "transport exit must await actual browser cleanup",
  );
  finishClose.resolve();
  await Promise.all([opening, closing]);
  assert.equal(session.browser, null);
  assert.equal(session.context, null);
});

test("concurrent close calls await the same browser shutdown", async () => {
  const finishClose = deferred();
  const launched = browser({ close: () => finishClose.promise });
  const session = new BrowserSession({
    chromium: { launch: async () => launched },
  });
  await session.ensureBrowser();
  const first = session.close();
  const second = session.close();
  assert.equal(first, second);
  await tick();
  assert.equal(launched.closeCalls, 1);
  finishClose.resolve();
  await Promise.all([first, second]);
});

test("stalled element disposal cannot block browser shutdown", async () => {
  const launched = browser();
  const session = new BrowserSession({
    chromium: { launch: async () => launched },
  });
  await session.ensureBrowser();
  let disposed = false;
  session.refs.set("stalled", {
    dispose() {
      disposed = true;
      return new Promise(() => {});
    },
  });
  await session.close();
  assert.equal(disposed, false);
  assert.equal(launched.closeCalls, 1);
  assert.equal(session.refs.size, 0);
});

test("context initialization failures close the already launched browser", async () => {
  const failure = new Error("context creation failed");
  const launched = browser({
    newContext: async () => {
      throw failure;
    },
  });
  const session = new BrowserSession({
    chromium: { launch: async () => launched },
  });
  await assert.rejects(session.ensureBrowser(), (error) => error === failure);
  assert.equal(launched.closeCalls, 1);
  assert.equal(session.browser, null);
  assert.equal(session.context, null);
  await session.close();
  assert.equal(launched.closeCalls, 1);
});

test("closing during context creation cannot restore a cancelled context", async () => {
  const createContext = deferred();
  const launched = browser({ newContext: () => createContext.promise });
  const session = new BrowserSession({
    chromium: { launch: async () => launched },
  });
  const opening = assert.rejects(session.ensureBrowser(), {
    code: "session_cancelled",
  });
  await tick();
  const closing = session.close();
  createContext.resolve(context());
  await Promise.all([opening, closing]);
  assert.equal(launched.closeCalls, 1);
  assert.equal(session.browser, null);
  assert.equal(session.context, null);
});

test(
  "MCP stdin EOF waits for pending fake launch cleanup before exiting",
  { timeout: 5000 },
  async (t) => {
    const directory = await mkdtemp(
      join(tmpdir(), "openharness-browser-lifecycle-"),
    );
    t.after(() => rm(directory, { recursive: true, force: true }));
    const marker = join(directory, "closed");
    const runtime = join(directory, "fake-runtime.mjs");
    await writeFile(
      runtime,
      `
    import { writeFile } from "node:fs/promises";
    export const chromium = {
      async launch() {
        process.stderr.write("launching\\n");
        await new Promise(resolve => setTimeout(resolve, 100));
        return { async close() { await writeFile(${JSON.stringify(marker)}, "closed"); } };
      }
    };
  `,
    );
    const child = spawn(
      process.execPath,
      [fileURLToPath(new URL("../server.mjs", import.meta.url))],
      {
        env: { ...process.env, OPENHARNESS_BROWSER_RUNTIME: runtime },
        stdio: ["pipe", "pipe", "pipe"],
      },
    );
    t.after(() => {
      if (child.exitCode === null) child.kill("SIGKILL");
    });
    const exited = once(child, "close");
    child.stdout.resume();
    const launching = once(child.stderr, "data");
    child.stdin.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize" })}\n`,
    );
    child.stdin.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "browser_open", arguments: { url: "https://fixture.invalid" } } })}\n`,
    );
    assert.match((await launching)[0].toString(), /launching/);
    child.stdin.end();
    const [code, signal] = await exited;
    assert.equal(signal, null);
    assert.equal(code, 0);
    assert.equal(await readFile(marker, "utf8"), "closed");
  },
);
