import assert from "node:assert/strict";
import { homedir, tmpdir } from "node:os";
import { test } from "node:test";
import { BrowserSession } from "../browser.mjs";

const failingSession = (failure) =>
  new BrowserSession({
    chromium: {
      async launch() {
        throw failure;
      },
    },
  });

test("missing selected executable reports preparation rather than a display problem", async () => {
  for (const binary of ["chromium", "chromium_headless_shell"]) {
    const session = failingSession(
      new Error(
        `browserType.launch: Executable doesn't exist at /cache/${binary}-1248/chrome`,
      ),
    );
    await assert.rejects(session.ensureBrowser(), (error) => {
      assert.equal(error.code, "browser_not_prepared");
      assert.match(error.message, /Prepare Browser/);
      assert.doesNotMatch(error.message, /graphical|permission|\/cache/);
      return true;
    });
  }
});

test("launch failures retain bounded diagnostics without private paths or launch commands", async () => {
  const session = failingSession(
    new Error(
      `browserType.launch: Process exited with code 127\n` +
        `Browser logs:\n<launching> /browser --sensitive-flag=private-value\n` +
        `\u001b[31m[pid=100][err] missing shared library\u001b[0m\n` +
        `${homedir()}/profile ${tmpdir()}/session\n` +
        `https://user:secret@example.test?token=secret\n` +
        `token=secret password=secret api_key=secret Authorization: Bearer secret\n` +
        "detail ".repeat(1000),
    ),
  );
  await assert.rejects(session.ensureBrowser(), (error) => {
    assert.equal(error.code, "browser_launch_failed");
    assert.match(error.message, /missing shared library/);
    assert.match(error.message, /\[local path\]/);
    assert.doesNotMatch(error.message, /private-value|secret|\u001b|graphical/);
    assert.ok(!error.message.includes(homedir()));
    assert.ok(!error.message.includes(tmpdir()));
    assert.ok(error.message.length < 1000);
    return true;
  });
});

test("only a display-specific failure recommends an active desktop session", async () => {
  const session = failingSession(new Error("Missing X server or $DISPLAY"));
  await assert.rejects(session.ensureBrowser(), (error) => {
    assert.equal(error.code, "browser_launch_failed");
    assert.match(error.message, /active desktop session/);
    return true;
  });
});

test("non-Error failures produce a useful bounded fallback", async () => {
  await assert.rejects(failingSession(null).ensureBrowser(), (error) => {
    assert.equal(error.code, "browser_launch_failed");
    assert.match(error.message, /No diagnostic was returned/);
    return true;
  });
});
