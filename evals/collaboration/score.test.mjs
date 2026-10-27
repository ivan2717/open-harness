import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createFixture, promptFor } from "./fixture.mjs";
import { EXPECTED, score } from "./score.mjs";

async function fixture(t, result = structuredClone(EXPECTED)) {
  const workspace = await mkdtemp(
    join(tmpdir(), "openharness-collaboration-score-"),
  );
  t.after(() => rm(workspace, { recursive: true, force: true }));
  await createFixture(workspace);
  await mkdir(join(workspace, "outputs"));
  await writeFile(
    join(workspace, "outputs/reconciliation.json"),
    JSON.stringify(result),
  );
  await writeFile(
    join(workspace, "outputs/report.md"),
    "# 对账报告\n\n依据 orders.csv、payments.csv、refunds.csv 及 rules.md，按付款 ID 去重后只统计有效订单的已付款记录，已结算且匹配的退款冲减净回款。原始材料保持不变。具体记录 ID 及金额见 reconciliation.json。",
  );
  return workspace;
}

test("correct result passes even if row and evidence ordering differs", async (t) => {
  const result = structuredClone(EXPECTED);
  result.orders.reverse();
  result.anomalies.unpaid_order_ids.reverse();
  const scored = await score(await fixture(t, result));
  assert.equal(scored.passed, true);
  assert.equal(scored.passedChecks, scored.totalChecks);
});

test("rejects double counted payment and false evidence despite plausible summary", async (t) => {
  const result = structuredClone(EXPECTED);
  result.orders[1].paid += 80;
  result.orders[1].payment_ids.push("P202");
  result.orders[2].refund_ids = ["R303"];
  const scored = await score(await fixture(t, result));
  assert.equal(scored.passed, false);
  for (const name of [
    "order_amounts:O102",
    "order_evidence:O102",
    "order_evidence:O103",
  ]) {
    assert.equal(
      scored.checks.find((check) => check.name === name).passed,
      false,
    );
  }
});

test("rejects missing anomalies, extra orders and altered source", async (t) => {
  const result = structuredClone(EXPECTED);
  result.anomalies.orphan_refund_ids = [];
  result.orders.push({ ...result.orders[0], order_id: "INVENTED" });
  const workspace = await fixture(t, result);
  await writeFile(join(workspace, "rules.md"), "Changed rules");
  const scored = await score(workspace);
  for (const name of [
    "anomalies:orphan_refund_ids",
    "all_orders_once",
    "source_unchanged:rules.md",
  ]) {
    assert.equal(
      scored.checks.find((check) => check.name === name).passed,
      false,
    );
  }
});

test("handles malformed and missing output without claiming success", async (t) => {
  const workspace = await fixture(t);
  await writeFile(join(workspace, "outputs/reconciliation.json"), "not json");
  await rm(join(workspace, "outputs/report.md"));
  const scored = await score(workspace);
  assert.equal(scored.passed, false);
  assert.equal(
    scored.checks.find((check) => check.name === "json_readable").passed,
    false,
  );
  assert.equal(
    scored.checks.find((check) => check.name === "report_present").passed,
    false,
  );
});

test("prompt modes differ only in orchestration and never include expected totals", () => {
  for (const mode of ["single", "auto", "delegated"]) {
    const prompt = promptFor(mode);
    assert(prompt.includes("outputs/reconciliation.json"));
    assert(!prompt.includes("425"));
    assert(!prompt.includes("475"));
  }
  assert.throws(() => promptFor("unknown"));
});
