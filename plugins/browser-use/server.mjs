import { BrowserError, BrowserSession } from "./browser.mjs";
import { tools } from "./schema.mjs";

const session = new BrowserSession({
  headless: process.argv.includes("--headless") ? true : undefined,
});
const versions = new Set([
  "2025-11-25",
  "2025-06-18",
  "2025-03-26",
  "2024-11-05",
]);
const MAX_LINE = 1024 * 1024;
let buffer = "";
let initialized = false;
let shuttingDown = false;
const pending = new Set();
const cancelled = new Set();

function send(value) {
  if (!shuttingDown) process.stdout.write(`${JSON.stringify(value)}\n`);
}

async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  // The host bounds shutdown and escalates if necessary. Exiting here on a
  // shorter timer can orphan Chromium while Playwright is still closing it.
  try {
    await session.close();
  } finally {
    process.exit(0);
  }
}

async function handle(request) {
  if (shuttingDown) return;
  if (
    !request ||
    request.jsonrpc !== "2.0" ||
    typeof request.method !== "string"
  ) {
    send({
      jsonrpc: "2.0",
      id: request?.id ?? null,
      error: { code: -32600, message: "Invalid request" },
    });
    return;
  }
  const { id, method, params = {} } = request;
  if (method === "notifications/cancelled") {
    if (pending.has(params.requestId)) {
      cancelled.add(params.requestId);
      await session.close();
    }
    return;
  }
  if (id === undefined) return;
  if (pending.has(id)) {
    send({
      jsonrpc: "2.0",
      id,
      error: { code: -32600, message: "Duplicate request id" },
    });
    return;
  }
  pending.add(id);
  try {
    let result;
    if (method === "initialize") {
      initialized = true;
      result = {
        protocolVersion: versions.has(params.protocolVersion)
          ? params.protocolVersion
          : "2025-11-25",
        capabilities: { tools: { listChanged: false } },
        serverInfo: { name: "openharness-browser", version: "0.1.0" },
      };
    } else if (!initialized) {
      send({
        jsonrpc: "2.0",
        id,
        error: { code: -32002, message: "Initialize first" },
      });
      return;
    } else if (method === "ping") result = {};
    else if (method === "tools/list") result = { tools };
    else if (method === "tools/call") {
      try {
        result = await session.call(params.name, params.arguments ?? {});
      } catch (error) {
        result = {
          isError: true,
          content: [
            {
              type: "text",
              text: JSON.stringify({
                error:
                  error instanceof BrowserError
                    ? error.code
                    : "invalid_request_or_unavailable_page",
                message:
                  error instanceof BrowserError
                    ? error.message
                    : "Check the tool arguments and read a fresh browser snapshot before acting.",
                dispatched: false,
                verified: false,
              }),
            },
          ],
        };
      }
    } else {
      send({
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: "Method not found" },
      });
      return;
    }
    if (!cancelled.has(id)) send({ jsonrpc: "2.0", id, result });
  } finally {
    pending.delete(id);
    cancelled.delete(id);
  }
}

process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  if (shuttingDown) return;
  buffer += chunk;
  let newline;
  while ((newline = buffer.indexOf("\n")) !== -1) {
    if (newline > MAX_LINE) {
      void shutdown();
      return;
    }
    const line = buffer.slice(0, newline);
    buffer = buffer.slice(newline + 1);
    if (!line.trim()) continue;
    let request;
    try {
      request = JSON.parse(line);
    } catch {
      send({
        jsonrpc: "2.0",
        id: null,
        error: { code: -32700, message: "Parse error" },
      });
      continue;
    }
    void handle(request).catch(() => {
      void shutdown();
    });
  }
  if (buffer.length > MAX_LINE) void shutdown();
});
process.stdin.on("end", () => {
  void shutdown();
});
process.stdin.on("error", () => {
  void shutdown();
});
process.stdout.on("error", () => {
  void shutdown();
});
process.on("SIGTERM", () => {
  void shutdown();
});
process.on("SIGINT", () => {
  void shutdown();
});
