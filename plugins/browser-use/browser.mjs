import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { validateArguments } from "./schema.mjs";

const MAX_REFS = 128;
const MAX_TEXT = 24_000;
const MAX_TREE = 16_000;
const CONTROLS =
  "a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=combobox],[role=textbox],[contenteditable=true]";
const textBlock = (data) => ({
  type: "text",
  text: JSON.stringify(data, (_, value) =>
    typeof value === "string" ? value.toWellFormed() : value,
  ),
});
const sameState = (a, b) => JSON.stringify(a) === JSON.stringify(b);

// JSON decoders such as serde_json reject isolated UTF-16 surrogates. Preserve
// complete code points when truncating and replace malformed source text.
const truncateText = (text, length) =>
  text
    .toWellFormed()
    .slice(0, length)
    .replace(/[\uD800-\uDBFF]$/u, "");

function textBoundary(text, offset) {
  const previous = text.charCodeAt(offset - 1);
  const current = text.charCodeAt(offset);
  return previous >= 0xd800 &&
    previous <= 0xdbff &&
    current >= 0xdc00 &&
    current <= 0xdfff
    ? offset - 1
    : offset;
}

export class BrowserError extends Error {
  constructor(code, message = code) {
    super(message);
    this.code = code;
  }
}

async function loadChromium() {
  try {
    if (process.env.OPENHARNESS_BROWSER_RUNTIME) {
      const module = await import(
        pathToFileURL(process.env.OPENHARNESS_BROWSER_RUNTIME).href
      );
      return module.chromium ?? module.default.chromium;
    }
    const require = createRequire(import.meta.url);
    return require("playwright-core").chromium;
  } catch {
    throw new BrowserError(
      "browser_runtime_missing",
      "Browser runtime is unavailable. Prepare Browser Use in Open Harness's Extensions page.",
    );
  }
}

export class BrowserSession {
  constructor({
    chromium,
    headless,
    platform = process.platform,
    environment = process.env,
  } = {}) {
    this.chromium = chromium;
    this.headless =
      headless ??
      (platform === "linux" &&
        !environment.DISPLAY &&
        !environment.WAYLAND_DISPLAY);
    this.browser = null;
    this.context = null;
    this.page = null;
    this.tabs = new Map();
    this.nextTab = 0;
    this.revision = 0;
    this.refs = new Map();
    this.observedState = null;
    this.screenshotRevision = null;
    this.queue = Promise.resolve();
    this.closedGeneration = 0;
    this.initializing = null;
    this.closing = null;
    this.browserClosures = new WeakMap();
  }

  // Preserve operation ordering even when a caller sends concurrent MCP calls.
  call(name, args = {}) {
    const generation = this.closedGeneration;
    const operation = this.queue.then(async () => {
      if (generation !== this.closedGeneration)
        throw new BrowserError(
          "session_cancelled",
          "The browser session was closed; observe a new session before continuing.",
        );
      validateArguments(name, args);
      if (name === "browser_open") return this.open(args);
      if (name === "browser_snapshot") {
        if (args.tab_id) {
          const page = this.tabs.get(args.tab_id);
          if (!page || page.isClosed()) throw new BrowserError("unknown_tab");
          this.page = page;
          await page.bringToFront();
        }
        return {
          content: [textBlock(await this.snapshot(args.text_offset ?? 0))],
        };
      }
      if (name === "browser_action") return this.action(args);
      if (name === "browser_screenshot") return this.screenshot();
      await this.close();
      return { content: [textBlock({ closed: true })] };
    });
    this.queue = operation.catch(() => {});
    return operation;
  }

  async ensureBrowser() {
    const generation = this.closedGeneration;
    if (this.closing) await this.closing;
    if (generation !== this.closedGeneration)
      throw new BrowserError("session_cancelled");
    if (this.initializing) return this.initializing;
    if (this.browser?.isConnected()) return;
    const initialization = this.initializeBrowser(generation);
    this.initializing = initialization;
    const clear = () => {
      if (this.initializing === initialization) this.initializing = null;
    };
    void initialization.then(clear, clear);
    return initialization;
  }

  async initializeBrowser(generation) {
    let browser;
    try {
      this.chromium ??= await loadChromium();
      if (generation !== this.closedGeneration)
        throw new BrowserError("session_cancelled");
      try {
        browser = await this.chromium.launch({
          headless: this.headless,
          timeout: 20_000,
        });
      } catch {
        throw new BrowserError(
          "browser_launch_failed",
          "Could not start the isolated browser. Prepare Browser Use in Extensions and check that a graphical session is available.",
        );
      }
      if (generation !== this.closedGeneration) {
        throw new BrowserError("session_cancelled");
      }
      this.browser = browser;
      const context = await browser.newContext({
        viewport: { width: 1280, height: 800 },
        acceptDownloads: false,
        serviceWorkers: "block",
      });
      if (generation !== this.closedGeneration)
        throw new BrowserError("session_cancelled");
      this.context = context;
      this.context.setDefaultTimeout(8_000);
      this.context.setDefaultNavigationTimeout(15_000);
      await this.context.addInitScript(() => {
        let version = 0;
        new MutationObserver(() => {
          version += 1;
        }).observe(document, {
          subtree: true,
          childList: true,
          characterData: true,
          attributes: true,
        });
        // User edits and focus moves do not necessarily mutate DOM attributes.
        for (const type of ["input", "change", "focusin", "focusout"]) {
          document.addEventListener(
            type,
            () => {
              version += 1;
            },
            true,
          );
        }
        Object.defineProperty(window, "__openHarnessPageState", {
          value: () => ({
            version,
            url: location.href,
            width: innerWidth,
            height: innerHeight,
            scrollX,
            scrollY,
          }),
          writable: false,
          configurable: false,
        });
      });
      if (generation !== this.closedGeneration)
        throw new BrowserError("session_cancelled");
      this.context.on("page", (page) => this.registerPage(page));
      browser.on("disconnected", () => {
        if (this.browser !== browser) return;
        this.browser = null;
        this.context = null;
        this.page = null;
        this.tabs.clear();
        this.invalidate();
      });
    } catch (error) {
      if (this.browser === browser) {
        this.browser = null;
        this.context = null;
      }
      if (browser) await this.closeBrowser(browser);
      throw error;
    }
  }

  closeBrowser(browser) {
    let closing = this.browserClosures.get(browser);
    if (!closing) {
      closing = Promise.resolve().then(() => browser.close());
      this.browserClosures.set(browser, closing);
    }
    return closing;
  }

  registerPage(page) {
    const id = `tab-${++this.nextTab}`;
    this.tabs.set(id, page);
    page.on("dialog", (dialog) => {
      void dialog.dismiss().catch(() => {});
    });
    page.on("close", () => {
      this.tabs.delete(id);
      if (this.page === page) {
        this.page =
          [...this.tabs.values()].find((entry) => !entry.isClosed()) ?? null;
        this.invalidate();
      }
    });
    // The user or the site can navigate outside a model action. Its old refs
    // cannot be used again, even when a later document has the same DOM shape.
    page.on("framenavigated", (frame) => {
      if (this.page === page && frame === page.mainFrame()) this.invalidate();
    });
  }

  invalidate() {
    this.observedState = null;
    this.screenshotRevision = null;
  }

  requirePage() {
    if (!this.page || this.page.isClosed())
      throw new BrowserError(
        "browser_not_open",
        "Open a page with browser_open first.",
      );
    return this.page;
  }

  async state(page = this.requirePage()) {
    try {
      return await page.evaluate(() => window.__openHarnessPageState());
    } catch {
      throw new BrowserError(
        "page_changing",
        "The page is navigating or changing. Read browser_snapshot before acting.",
      );
    }
  }

  async releaseRefs() {
    const refs = [...this.refs.values()];
    this.refs.clear();
    await Promise.allSettled(refs.map((handle) => handle.dispose()));
  }

  async controls(page) {
    const list = await page.evaluateHandle(
      ({ selector, limit }) => {
        const inViewport = [];
        const outsideViewport = [];
        for (const element of document.querySelectorAll(selector)) {
          const style = getComputedStyle(element);
          if (
            !element.getClientRects().length ||
            style.visibility === "hidden" ||
            style.display === "none"
          )
            continue;
          const bounds = element.getBoundingClientRect();
          const visible =
            bounds.bottom > 0 &&
            bounds.top < innerHeight &&
            bounds.right > 0 &&
            bounds.left < innerWidth;
          const target = visible ? inViewport : outsideViewport;
          if (target.length <= limit) target.push(element);
        }
        return [...inViewport, ...outsideViewport].slice(0, limit + 1);
      },
      { selector: CONTROLS, limit: MAX_REFS },
    );
    try {
      const properties = await list.getProperties();
      return [...properties.values()]
        .map((value) => value.asElement())
        .filter(Boolean);
    } finally {
      await list.dispose();
    }
  }

  async snapshot(textOffset = 0) {
    const page = this.requirePage();
    this.invalidate();
    await this.releaseRefs();
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const start = await this.state(page);
      const handles = await this.controls(page);
      const selected = handles.slice(0, MAX_REFS);
      const elements = await Promise.all(
        selected.map(async (handle, index) => {
          const details = await handle.evaluate((element) => {
            const style = getComputedStyle(element);
            if (
              !element.getClientRects().length ||
              style.visibility === "hidden" ||
              style.display === "none"
            )
              return null;
            const tag = element.tagName.toLowerCase();
            const labelled = (element.getAttribute("aria-labelledby") ?? "")
              .split(/\s+/)
              .filter(Boolean)
              .map((id) => document.getElementById(id)?.textContent ?? "")
              .join(" ");
            const name =
              element.getAttribute("aria-label") ||
              labelled ||
              Array.from(element.labels ?? [])
                .map((label) => label.textContent)
                .join(" ") ||
              element.innerText ||
              element.getAttribute("placeholder") ||
              element.getAttribute("title") ||
              "";
            const role =
              element.getAttribute("role") ||
              {
                a: "link",
                button: "button",
                input: "input",
                textarea: "textbox",
                select: "combobox",
                summary: "button",
              }[tag] ||
              tag;
            return {
              role,
              name: name
                .trim()
                .toWellFormed()
                .slice(0, 500)
                .replace(/[\uD800-\uDBFF]$/u, ""),
              disabled:
                Boolean(element.disabled) ||
                element.getAttribute("aria-disabled") === "true",
              ...(element.type === "password" ? { sensitive: true } : {}),
            };
          });
          return details ? { ref: `e${index + 1}`, ...details } : null;
        }),
      );
      let tree;
      let body;
      try {
        [tree, body] = await Promise.all([
          page.locator("body").ariaSnapshot({ timeout: 5_000 }),
          page.locator("body").innerText({ timeout: 5_000 }),
        ]);
      } catch {
        await Promise.allSettled(handles.map((handle) => handle.dispose()));
        throw new BrowserError(
          "page_unavailable",
          "The page has no readable document yet. Observe again before acting.",
        );
      }
      body = body.toWellFormed();
      if (textOffset > body.length) {
        await Promise.allSettled(handles.map((handle) => handle.dispose()));
        throw new BrowserError(
          "invalid_text_offset",
          "Page text has changed or this offset is past its end. Read browser_snapshot with text_offset 0.",
        );
      }
      // A caller-provided cursor may bisect an emoji. Include the complete
      // character and report the adjusted cursor rather than dropping it.
      textOffset = textBoundary(body, textOffset);
      const end = await this.state(page);
      if (!sameState(start, end)) {
        await Promise.allSettled(handles.map((handle) => handle.dispose()));
        if (attempt === 0) continue;
        throw new BrowserError(
          "page_changing",
          "Page changed while reading. Request a fresh snapshot before acting.",
        );
      }
      const visible = elements.filter(Boolean);
      const keep = new Set(
        visible.map((element) => Number(element.ref.slice(1)) - 1),
      );
      for (let index = 0; index < handles.length; index += 1) {
        if (keep.has(index)) this.refs.set(`e${index + 1}`, handles[index]);
        else await handles[index].dispose();
      }
      this.observedState = end;
      this.revision += 1;
      const tabs = [...this.tabs.entries()]
        .filter(([, entry]) => !entry.isClosed())
        .map(([id, entry]) => ({
          tab_id: id,
          url: entry.url(),
          active: entry === page,
        }));
      const text = truncateText(body.slice(textOffset), MAX_TEXT);
      const accessibility = truncateText(tree, MAX_TREE);
      const snapshot = {
        revision: this.revision,
        tab_id: tabs.find((tab) => tab.active)?.tab_id,
        url: truncateText(page.url(), 2048),
        title: truncateText(await page.title(), 256),
        viewport: { width: end.width, height: end.height },
        tabs,
        text,
        text_offset: textOffset,
        next_text_offset: null,
        accessibility,
        elements: visible,
        truncated:
          body.length > textOffset + text.length ||
          tree.length > accessibility.length ||
          handles.length > MAX_REFS,
      };
      // The host caps text tool results at 32 KiB. Bound the complete nested
      // MCP JSON, including escaping and UTF-8, so large pages never lose
      // their refs/revision inside a generic truncated text preview.
      const outputBytes = () =>
        Buffer.byteLength(
          JSON.stringify({
            content: [
              textBlock({ dispatched: true, verified: false, snapshot }),
            ],
          }),
        );
      while (outputBytes() > 28 * 1024) {
        snapshot.truncated = true;
        if (snapshot.text.length > 512)
          snapshot.text = truncateText(
            snapshot.text,
            Math.floor(snapshot.text.length / 2),
          );
        else if (snapshot.accessibility.length > 512)
          snapshot.accessibility = truncateText(
            snapshot.accessibility,
            Math.floor(snapshot.accessibility.length / 2),
          );
        else if (snapshot.elements.length > 1) {
          const removed = snapshot.elements.pop();
          await this.refs.get(removed.ref)?.dispose();
          this.refs.delete(removed.ref);
        } else if (snapshot.tabs.length > 1) {
          const index = snapshot.tabs.findIndex((tab) => !tab.active);
          snapshot.tabs.splice(index, 1);
        } else {
          snapshot.tabs[0].url = truncateText(snapshot.tabs[0].url, 2048);
          break;
        }
      }
      const nextOffset = textOffset + snapshot.text.length;
      snapshot.next_text_offset = nextOffset < body.length ? nextOffset : null;
      return snapshot;
    }
  }

  async open({ url, new_tab: newTab = false }) {
    let target;
    try {
      target = new URL(url);
    } catch {
      throw new BrowserError("invalid_url", "Use an absolute HTTP(S) URL.");
    }
    if (
      !["http:", "https:"].includes(target.protocol) ||
      target.username ||
      target.password
    )
      throw new BrowserError(
        "invalid_url",
        "Only HTTP(S) URLs without embedded credentials are supported.",
      );
    await this.ensureBrowser();
    if (newTab || !this.page || this.page.isClosed())
      this.page = await this.context.newPage();
    this.invalidate();
    try {
      await this.page.goto(target.href, { waitUntil: "domcontentloaded" });
    } catch {
      return this.uncertainResult(
        "navigation_failed",
        "Navigation failed or timed out; it may still have changed the page. Observe before navigating again.",
      );
    }
    return this.actionResult();
  }

  async action(args) {
    const page = this.requirePage();
    if (
      args.revision !== this.revision ||
      !this.observedState ||
      !sameState(await this.state(page), this.observedState)
    ) {
      this.invalidate();
      throw new BrowserError(
        "stale_snapshot",
        "Page state changed. Read browser_snapshot and choose the action again.",
      );
    }
    const ref = args.ref ? this.refs.get(args.ref) : null;
    if (args.ref && !ref)
      throw new BrowserError(
        "unknown_ref",
        "This element does not appear in the current snapshot.",
      );
    if (args.action === "click" && !ref) {
      if (this.screenshotRevision !== args.revision)
        throw new BrowserError(
          "screenshot_required",
          "Read browser_screenshot before using coordinates.",
        );
      if (
        args.x >= this.observedState.width ||
        args.y >= this.observedState.height
      )
        throw new BrowserError(
          "invalid_coordinates",
          "Coordinates must be inside the screenshot viewport.",
        );
    }
    this.invalidate();
    try {
      switch (args.action) {
        case "click":
          if (ref) await ref.click();
          else await page.mouse.click(args.x, args.y);
          break;
        case "fill":
          await ref.fill(args.text);
          break;
        case "select":
          await ref.selectOption(args.text);
          break;
        case "press":
          await ref.press(args.key);
          break;
        case "scroll":
          await page.mouse.wheel(0, args.delta_y);
          break;
        case "back":
          await page.goBack({ waitUntil: "domcontentloaded" });
          break;
        case "forward":
          await page.goForward({ waitUntil: "domcontentloaded" });
          break;
      }
    } catch {
      return this.uncertainResult(
        "action_failed",
        "The action failed or timed out. Its effect is uncertain; observe before deciding whether another action is needed.",
      );
    }
    return this.actionResult();
  }

  async actionResult() {
    try {
      return {
        content: [
          textBlock({
            dispatched: true,
            verified: false,
            snapshot: await this.snapshot(),
          }),
        ],
      };
    } catch {
      return {
        content: [
          textBlock({
            dispatched: true,
            verified: false,
            observation_error:
              "Page changed or could not be read after the action. Read browser_snapshot before continuing.",
          }),
        ],
      };
    }
  }

  async uncertainResult(code, message) {
    let snapshot;
    try {
      snapshot = await this.snapshot();
    } catch {
      /* Preserve uncertainty without retrying an action. */
    }
    return {
      isError: true,
      content: [
        textBlock({
          error: code,
          message,
          dispatched: "unknown",
          verified: false,
          ...(snapshot ? { snapshot } : {}),
        }),
      ],
    };
  }

  async screenshot() {
    const snapshot = await this.snapshot();
    // Do not inject temporary CSS to hide carets or stop animations: those DOM
    // mutations would invalidate the state paired with the screenshot.
    const image = await this.requirePage().screenshot({
      type: "jpeg",
      quality: 75,
      animations: "allow",
      caret: "initial",
      timeout: 8_000,
    });
    if (!sameState(await this.state(), this.observedState)) {
      this.invalidate();
      throw new BrowserError(
        "stale_snapshot",
        "Page changed while taking the screenshot. Capture another screenshot before using coordinates.",
      );
    }
    if (image.length > 2 * 1024 * 1024)
      throw new BrowserError("screenshot_too_large");
    this.screenshotRevision = snapshot.revision;
    return {
      content: [
        textBlock(snapshot),
        {
          type: "image",
          mimeType: "image/jpeg",
          data: image.toString("base64"),
        },
      ],
    };
  }

  // May run outside the serialized queue on cancellation or transport loss.
  // Closing the context stops pending browser operations; no action is replayed.
  close() {
    this.closedGeneration += 1;
    if (this.closing) return this.closing;
    const browser = this.browser;
    this.browser = null;
    this.context = null;
    this.page = null;
    this.tabs.clear();
    this.invalidate();
    // Closing the browser releases all handles. Disposing each handle first can
    // hang on a stalled connection and prevent the browser from ever closing.
    this.refs.clear();
    const closing = Promise.all([
      browser ? this.closeBrowser(browser) : undefined,
      // A launch can finish after cancellation; initializeBrowser closes that
      // late browser before rejecting. Transport shutdown must wait for it.
      this.initializing?.catch(() => {}),
    ]).then(() => {});
    this.closing = closing;
    const clear = () => {
      if (this.closing === closing) this.closing = null;
    };
    void closing.then(clear, clear);
    return closing;
  }
}
