import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { BrowserSession } from "../browser.mjs";
import { validateArguments } from "../schema.mjs";

const desktopRequire = createRequire(
  new URL("../../../apps/desktop/package.json", import.meta.url),
);
const runtime =
  process.env.OPENHARNESS_BROWSER_RUNTIME ||
  desktopRequire.resolve("playwright-core");
const { chromium } = desktopRequire(runtime);
const data = (result) =>
  JSON.parse(result.content.find((item) => item.type === "text").text);

function assertUnicodeJson(result) {
  const visit = (value) => {
    if (typeof value === "string")
      assert(value.isWellFormed(), "JSON strings must contain Unicode scalars");
    else if (value && typeof value === "object")
      Object.values(value).forEach(visit);
  };
  // Check both JSON layers. JSON.parse alone accepts isolated surrogates that
  // strict decoders such as Rust's serde_json cannot deserialize into strings.
  visit(JSON.parse(JSON.stringify(result)));
  const parsed = data(result);
  visit(parsed);
  return parsed;
}

async function fixture(t) {
  let submitted = 0;
  const server = createServer((request, response) => {
    if (request.url === "/submit") {
      submitted += 1;
      response.end(
        `<html><body><h1>Saved</h1><p>Submission ${submitted}</p><a href="/">Return</a></body></html>`,
      );
    } else {
      response.setHeader("Content-Type", "text/html; charset=utf-8");
      response.end(`<html><head><title>Browser fixture</title></head><body>
        <h1>Local browser fixture</h1>
        <form method="post" action="/submit"><label>Email<input name="email" type="email"></label><button>Save</button></form>
        <label for="choice">Choice</label><select id="choice"><option value="a">Alpha</option><option value="b">Beta</option></select>
        <a href="/second" target="_blank">Open tab</a>
        <button disabled>Disabled</button>
        <div style="height:1600px">Long content</div><p>Page end</p>
      </body></html>`);
    }
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const session = new BrowserSession({ chromium, headless: true });
  t.after(() => session.close());
  return {
    session,
    url: `http://127.0.0.1:${server.address().port}`,
    submitted: () => submitted,
  };
}

test("real browser: semantic read, fill, submit, navigation, screenshot and clean restart", async (t) => {
  const { session, url, submitted } = await fixture(t);
  const opened = data(await session.call("browser_open", { url }));
  assert.equal(opened.dispatched, true);
  assert.equal(opened.verified, false);
  assert.match(opened.snapshot.accessibility, /Local browser fixture/);
  assert.match(opened.snapshot.text, /Long content/);
  const email = opened.snapshot.elements.find(
    (element) => element.name === "Email",
  );
  assert.ok(email);
  const filled = data(
    await session.call("browser_action", {
      action: "fill",
      revision: opened.snapshot.revision,
      ref: email.ref,
      text: "local@example.test",
    }),
  );
  assert.equal(
    await session.page.locator("input").inputValue(),
    "local@example.test",
  );
  await assert.rejects(
    session.call("browser_action", {
      action: "click",
      revision: opened.snapshot.revision,
      ref: email.ref,
    }),
    /Page state changed/,
  );
  const fresh = data(await session.call("browser_snapshot"));
  assert.ok(fresh.revision > filled.snapshot.revision);
  const save = fresh.elements.find((element) => element.name === "Save");
  const saved = data(
    await session.call("browser_action", {
      action: "click",
      revision: fresh.revision,
      ref: save.ref,
    }),
  );
  assert.equal(submitted(), 1);
  assert.match(saved.snapshot.text, /Submission 1/);
  assert.match(saved.snapshot.url, /\/submit$/);

  const screenshot = await session.call("browser_screenshot");
  const image = screenshot.content.find((item) => item.type === "image");
  assert.equal(image.mimeType, "image/jpeg");
  assert.ok(Buffer.from(image.data, "base64").length > 1000);
  const metadata = data(screenshot);
  assert.equal(metadata.viewport.width, 1280);
  await assert.rejects(
    session.call("browser_action", {
      action: "click",
      revision: metadata.revision,
      x: 1281,
      y: 10,
    }),
    /Coordinates must/,
  );
  const clicked = data(
    await session.call("browser_action", {
      action: "click",
      revision: metadata.revision,
      x: 1200,
      y: 700,
    }),
  );
  assert.equal(clicked.dispatched, true);
  assert.equal(submitted(), 1);

  const browser = session.browser;
  const page = session.page;
  await session.context.addCookies([
    { name: "isolated", value: "temporary", url },
  ]);
  assert.ok((await session.context.cookies()).length);
  assert.equal(data(await session.call("browser_close")).closed, true);
  assert.equal(browser.isConnected(), false);
  assert.equal(page.isClosed(), true);
  await session.call("browser_open", { url });
  assert.deepEqual(await session.context.cookies(), []);
});

test("state changes invalidate references and coordinate clicks require a current screenshot", async (t) => {
  const { session, url } = await fixture(t);
  const { snapshot } = data(await session.call("browser_open", { url }));
  await session.page.evaluate(() => {
    document.querySelector("h1").textContent = "Changed by another actor";
  });
  await assert.rejects(
    session.call("browser_action", {
      action: "click",
      revision: snapshot.revision,
      ref: snapshot.elements[0].ref,
    }),
    /Page state changed/,
  );
  const fresh = data(await session.call("browser_snapshot"));
  await assert.rejects(
    session.call("browser_action", {
      action: "click",
      revision: fresh.revision,
      x: 1,
      y: 1,
    }),
    /Read browser_screenshot/,
  );
  const image = data(await session.call("browser_screenshot"));
  await session.page.setViewportSize({ width: 1000, height: 600 });
  await assert.rejects(
    session.call("browser_action", {
      action: "click",
      revision: image.revision,
      x: 1,
      y: 1,
    }),
    /Page state changed/,
  );
  await session.page.goto(`${url}/second`);
  await assert.rejects(
    session.call("browser_action", {
      action: "click",
      revision: image.revision,
      x: 1,
      y: 1,
    }),
    /Page state changed/,
  );
});

test("concurrent actions are serialized and cannot reuse a prior snapshot", async (t) => {
  const { session, url } = await fixture(t);
  const { snapshot } = data(await session.call("browser_open", { url }));
  const email = snapshot.elements.find((element) => element.name === "Email");
  const results = await Promise.allSettled([
    session.call("browser_action", {
      action: "fill",
      revision: snapshot.revision,
      ref: email.ref,
      text: "first@example.test",
    }),
    session.call("browser_action", {
      action: "fill",
      revision: snapshot.revision,
      ref: email.ref,
      text: "second@example.test",
    }),
  ]);
  assert.equal(results[0].status, "fulfilled");
  assert.equal(results[1].status, "rejected");
  assert.equal(
    await session.page.locator("input").inputValue(),
    "first@example.test",
  );
});

test("new tabs can be observed and failed operations are not replayed", async (t) => {
  const { session, url, submitted } = await fixture(t);
  const { snapshot } = data(await session.call("browser_open", { url }));
  const popup = snapshot.elements.find(
    (element) => element.name === "Open tab",
  );
  const newPage = session.context.waitForEvent("page");
  await session.call("browser_action", {
    action: "click",
    revision: snapshot.revision,
    ref: popup.ref,
  });
  await (await newPage).waitForLoadState("domcontentloaded");
  const current = data(await session.call("browser_snapshot"));
  assert.equal(current.tabs.length, 2);
  const second = current.tabs.find((tab) => !tab.active);
  const selected = data(
    await session.call("browser_snapshot", { tab_id: second.tab_id }),
  );
  assert.equal(selected.tab_id, second.tab_id);
  const disabled = selected.elements.find(
    (element) => element.name === "Disabled",
  );
  session.context.setDefaultTimeout(100);
  const failed = await session.call("browser_action", {
    action: "click",
    revision: selected.revision,
    ref: disabled.ref,
  });
  assert.equal(failed.isError, true);
  assert.equal(data(failed).dispatched, "unknown");
  assert.equal(submitted(), 0);
});

test("select, keyboard, scroll and history navigation execute in the real browser", async (t) => {
  const { session, url } = await fixture(t);
  let { snapshot } = data(await session.call("browser_open", { url }));
  const choice = snapshot.elements.find((element) => element.name === "Choice");
  ({ snapshot } = data(
    await session.call("browser_action", {
      action: "select",
      revision: snapshot.revision,
      ref: choice.ref,
      text: "b",
    }),
  ));
  assert.equal(await session.page.locator("select").inputValue(), "b");
  const email = snapshot.elements.find((element) => element.name === "Email");
  ({ snapshot } = data(
    await session.call("browser_action", {
      action: "press",
      revision: snapshot.revision,
      ref: email.ref,
      key: "Tab",
    }),
  ));
  assert.equal(
    await session.page.evaluate(() => document.activeElement.textContent),
    "Save",
  );
  const scrolled = data(
    await session.call("browser_action", {
      action: "scroll",
      revision: snapshot.revision,
      delta_y: 600,
    }),
  );
  assert.equal(scrolled.dispatched, true);
  await session.page.waitForFunction(() => scrollY > 0);
  ({ snapshot } = data(
    await session.call("browser_open", { url: `${url}/second` }),
  ));
  ({ snapshot } = data(
    await session.call("browser_action", {
      action: "back",
      revision: snapshot.revision,
    }),
  ));
  assert.equal(new URL(snapshot.url).pathname, "/");
  ({ snapshot } = data(
    await session.call("browser_action", {
      action: "forward",
      revision: snapshot.revision,
    }),
  ));
  assert.equal(new URL(snapshot.url).pathname, "/second");
});

test("cancellation closes the browser and invalidates already queued operations", async (t) => {
  const { session, url } = await fixture(t);
  const { snapshot } = data(await session.call("browser_open", { url }));
  const browser = session.browser;
  const disabled = snapshot.elements.find(
    (element) => element.name === "Disabled",
  );
  const waiting = session.call("browser_action", {
    action: "click",
    revision: snapshot.revision,
    ref: disabled.ref,
  });
  const queued = session.call("browser_snapshot");
  const rejected = assert.rejects(queued, /browser session was closed/);
  await new Promise((resolve) => setTimeout(resolve, 50));
  await session.close();
  await waiting;
  await rejected;
  assert.equal(browser.isConnected(), false);
  assert.equal(session.browser, null);
});

test("MCP stdio performs a real roundtrip and exits cleanly on EOF", async (t) => {
  const { url } = await fixture(t);
  const child = spawn(
    process.execPath,
    [fileURLToPath(new URL("../server.mjs", import.meta.url)), "--headless"],
    {
      env: { ...process.env, OPENHARNESS_BROWSER_RUNTIME: runtime },
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  t.after(() => child.kill());
  let id = 0;
  let buffer = "";
  const pending = new Map();
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    let newline;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const message = JSON.parse(buffer.slice(0, newline));
      buffer = buffer.slice(newline + 1);
      pending.get(message.id)?.(message);
      pending.delete(message.id);
    }
  });
  const rpc = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const requestId = ++id;
      const timer = setTimeout(
        () => reject(new Error("MCP roundtrip timed out")),
        25_000,
      );
      pending.set(requestId, (result) => {
        clearTimeout(timer);
        resolve(result);
      });
      child.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id: requestId, method, params })}\n`,
      );
    });
  const initialized = await rpc("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "browser-test", version: "1" },
  });
  assert.equal(initialized.result.protocolVersion, "2025-11-25");
  assert.equal((await rpc("tools/list")).result.tools.length, 5);
  const opened = await rpc("tools/call", {
    name: "browser_open",
    arguments: { url },
  });
  assert.match(data(opened.result).snapshot.text, /Local browser fixture/);
  const screenshot = await rpc("tools/call", {
    name: "browser_screenshot",
    arguments: {},
  });
  assert.ok(screenshot.result.content.some((block) => block.type === "image"));
  const exiting = once(child, "exit");
  child.stdin.end();
  const [code] = await exiting;
  assert.equal(code, 0);
});

test("argument validation rejects ambiguous or unrelated action parameters", () => {
  assert.throws(
    () =>
      validateArguments("browser_action", {
        action: "fill",
        revision: 1,
        ref: "e1",
      }),
    /requires_ref_and_text/,
  );
  assert.throws(
    () =>
      validateArguments("browser_action", {
        action: "click",
        revision: 1,
        ref: "e1",
        x: 1,
        y: 2,
      }),
    /requires_ref_or_coordinates/,
  );
  assert.throws(
    () =>
      validateArguments("browser_action", {
        action: "back",
        revision: 1,
        text: "ignored",
      }),
    /invalid_arguments_for_action/,
  );
  assert.throws(
    () =>
      validateArguments("browser_action", {
        action: "scroll",
        revision: 1,
        delta_y: Infinity,
      }),
    /invalid_argument/,
  );
  assert.throws(
    () =>
      validateArguments("browser_open", {
        url: "http://localhost",
        javascript: "alert(1)",
      }),
    /unexpected_argument/,
  );
});

test("Linux without a graphical session falls back to headless without changing other platform defaults", () => {
  assert.equal(
    new BrowserSession({ platform: "linux", environment: {} }).headless,
    true,
  );
  assert.equal(
    new BrowserSession({
      platform: "linux",
      environment: { DISPLAY: "", WAYLAND_DISPLAY: "" },
    }).headless,
    true,
  );
  assert.equal(
    new BrowserSession({ platform: "linux", environment: { DISPLAY: ":0" } })
      .headless,
    false,
  );
  assert.equal(
    new BrowserSession({
      platform: "linux",
      environment: { WAYLAND_DISPLAY: "wayland-0" },
    }).headless,
    false,
  );
  assert.equal(
    new BrowserSession({ platform: "darwin", environment: {} }).headless,
    false,
  );
  assert.equal(
    new BrowserSession({ platform: "darwin", environment: {}, headless: true })
      .headless,
    true,
  );
});

test("large multilingual snapshots retain actionable structure inside the host text budget", async (t) => {
  const { session, url } = await fixture(t);
  await session.call("browser_open", { url });
  await session.page.evaluate(() => {
    const div = document.createElement("div");
    div.textContent = '复杂页面\\"\n'.repeat(15000);
    document.body.appendChild(div);
    for (let index = 0; index < 140; index += 1) {
      const button = document.createElement("button");
      button.textContent = `操作 ${index} ${"内容".repeat(250)}`;
      document.body.appendChild(button);
    }
  });
  const result = await session.call("browser_snapshot");
  const snapshot = data(result);
  assert.equal(snapshot.truncated, true);
  assert(Buffer.byteLength(JSON.stringify(result)) < 32 * 1024);
  assert(snapshot.elements.length > 0);
  const email = snapshot.elements.find((element) => element.name === "Email");
  const filled = await session.call("browser_action", {
    action: "fill",
    revision: snapshot.revision,
    ref: email.ref,
    text: "bounded@example.test",
  });
  assert.equal(data(filled).dispatched, true);
  assert(Buffer.byteLength(JSON.stringify(filled)) < 32 * 1024);
  assert(snapshot.next_text_offset > 0);
  const next = data(
    await session.call("browser_snapshot", {
      text_offset: snapshot.next_text_offset,
    }),
  );
  const body = await session.page.locator("body").innerText();
  assert.equal(
    next.text,
    body.slice(next.text_offset, next.text_offset + next.text.length),
  );
  assert.equal(next.text_offset, snapshot.next_text_offset);
  const endOffset = body.length - 80;
  const tail = data(
    await session.call("browser_snapshot", { text_offset: endOffset }),
  );
  assert.equal(tail.text, body.slice(endOffset));
  assert.equal(tail.next_text_offset, null);
  await assert.rejects(
    session.call("browser_snapshot", { text_offset: body.length + 1 }),
    /offset is past its end/,
  );
  await session.page.locator("button").last().scrollIntoViewIfNeeded();
  const bottom = data(await session.call("browser_snapshot"));
  assert(
    bottom.elements.some((element) => element.name.startsWith("操作 139")),
    "scrolling must expose controls beyond the initial ref limit",
  );
});

test("real emoji pages preserve Unicode across labels, snapshot truncation and pagination", async (t) => {
  const { session, url } = await fixture(t);
  await session.call("browser_open", { url });
  await session.page.evaluate(() => {
    document.title = `${"T".repeat(255)}😀 title`;
    document.body.replaceChildren();
    const paragraph = document.createElement("p");
    paragraph.textContent = "a".repeat(50_000);
    const input = document.createElement("input");
    input.setAttribute("aria-label", `${"L".repeat(499)}😀 label`);
    const broken = document.createElement("button");
    broken.setAttribute("aria-label", "broken\ud800label");
    document.body.append(paragraph, input, broken);
  });
  const baseline = assertUnicodeJson(await session.call("browser_snapshot"));
  assert.equal(baseline.title, "T".repeat(255));
  assert.equal(baseline.elements[0].name, "L".repeat(499));
  assert.equal(baseline.elements[1].name, "broken�label");
  const originalTree = await session.page.locator("body").ariaSnapshot();
  const treePrefix = originalTree.indexOf("a".repeat(64));
  assert(treePrefix >= 0);
  assert.equal(baseline.accessibility.length, 16_000);
  // Place emojis across the initial text/AX limits and the actual byte-budget
  // truncation point observed from the real page, preserving UTF-16 length.
  const boundaries = [
    23_999,
    baseline.text.length - 1,
    baseline.accessibility.length - treePrefix - 1,
  ];
  await session.page.evaluate((positions) => {
    const text = Array(50_000).fill("a");
    for (const position of positions) {
      text[position] = "😀";
      text[position + 1] = "";
    }
    document.querySelector("p").textContent = text.join("");
  }, boundaries);
  const body = await session.page.locator("body").innerText();
  let snapshot = assertUnicodeJson(await session.call("browser_snapshot"));
  assert.equal(snapshot.accessibility.length, 15_999);
  const chunks = [];
  for (let page = 0; page < 100; page += 1) {
    chunks.push(snapshot.text);
    assert.equal(
      snapshot.text,
      body.slice(
        snapshot.text_offset,
        snapshot.text_offset + snapshot.text.length,
      ),
    );
    if (snapshot.next_text_offset === null) break;
    assert(snapshot.next_text_offset > snapshot.text_offset);
    assert(
      !/^[\uDC00-\uDFFF]/u.test(body.slice(snapshot.next_text_offset)),
      "a returned cursor must not bisect an emoji",
    );
    snapshot = assertUnicodeJson(
      await session.call("browser_snapshot", {
        text_offset: snapshot.next_text_offset,
      }),
    );
  }
  assert.equal(snapshot.next_text_offset, null);
  assert.equal(
    chunks.join(""),
    body,
    "pagination must not lose or replace emojis",
  );
  const bisected = assertUnicodeJson(
    await session.call("browser_snapshot", { text_offset: boundaries[0] + 1 }),
  );
  assert.equal(bisected.text_offset, boundaries[0]);
  assert(bisected.text.startsWith("😀"));

  // Many long controls force the JSON budget to halve the accessibility tree.
  // Verify that this second truncation path also retains whole code points.
  await session.page.evaluate(() => {
    document.querySelector("p").textContent = "a".repeat(50_000);
    for (let index = 0; index < 128; index += 1) {
      const input = document.createElement("input");
      input.setAttribute("aria-label", "control".repeat(80));
      document.body.append(input);
    }
  });
  const crowded = assertUnicodeJson(await session.call("browser_snapshot"));
  assert(crowded.accessibility.length < 16_000);
  await session.page.evaluate(
    (position) => {
      document.querySelector("p").textContent =
        `${"a".repeat(position)}😀${"a".repeat(50_000 - position - 2)}`;
    },
    crowded.accessibility.length - treePrefix - 1,
  );
  const trimmed = assertUnicodeJson(await session.call("browser_snapshot"));
  assert.equal(trimmed.accessibility.length, crowded.accessibility.length - 1);
});

test("host website permissions protect navigation, changed pages and screenshots", async (t) => {
  const { session, url } = await fixture(t);
  const other = await fixture(t);
  session.requireHostPermission = true;
  const approved = { origins: [new URL(url).origin] };
  await assert.rejects(session.call("browser_open", { url }), /host must approve/i);
  const opened = data(await session.call("browser_open", { url }, approved));
  assert.match(opened.snapshot.text, /Local browser fixture/);
  await assert.rejects(session.call("browser_open", { url: other.url }, approved), /not approved the navigation target/);
  assert.equal(new URL(session.page.url()).origin, new URL(url).origin);
  await session.page.goto(other.url);
  await assert.rejects(session.call("browser_snapshot", {}, approved), /No page content was returned/);
  await assert.rejects(session.call("browser_screenshot", {}, approved), /No page content was returned/);
  await assert.rejects(session.call("browser_action", { action: "scroll", delta_y: 100, revision: opened.snapshot.revision }, approved), /No page content was returned/);
  const target = data(await session.call("__openharness_browser_target", {}));
  assert.deepEqual(target, { origin: new URL(other.url).origin });
  const authorized = data(await session.call("browser_snapshot", {}, { origins: [target.origin] }));
  assert.match(authorized.text, /Local browser fixture/);
  await session.call("browser_open", { url }, { origins: ["*"] });
  await session.call("browser_close", {}, { origins: [] });
});

test("password and file fills are refused without dispatch, ordinary inputs still work", async (t) => {
  const { session, url } = await fixture(t);
  await session.call("browser_open", { url });
  await session.page.setContent('<label>Password<input type="password" id="password"></label><label>File<input type="file" id="file"></label><label>Text<input id="text"></label>');
  for (const name of ["Password", "File"]) {
    const snapshot = data(await session.call("browser_snapshot"));
    const element = snapshot.elements.find(entry => entry.name === name);
    assert.ok(element);
    await assert.rejects(session.call("browser_action", { action: "fill", ref: element.ref, revision: snapshot.revision, text: "must-not-be-written" }), /must be completed by the user/);
    assert.equal(await session.page.locator(`#${name.toLowerCase()}`).inputValue(), "");
  }
  const snapshot = data(await session.call("browser_snapshot"));
  const element = snapshot.elements.find(entry => entry.name === "Text");
  await session.call("browser_action", { action: "fill", ref: element.ref, revision: snapshot.revision, text: "ordinary text" });
  assert.equal(await session.page.locator("#text").inputValue(), "ordinary text");
});
