import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { INPUTS } from "./fixture.mjs";

// This file must stay outside the evaluated workspace. It is never included in
// prompts. Values below were calculated from the fixed fixture independently.
export const EXPECTED = Object.freeze({
  summary: {
    order_total: 700,
    matched_paid: 475,
    applied_refund: 50,
    net_received: 425,
    unpaid_before_refunds: 225,
  },
  orders: [
    {
      order_id: "O101",
      paid: 120,
      refund: 20,
      net: 100,
      unpaid_before_refunds: 0,
      payment_ids: ["P201"],
      refund_ids: ["R302"],
    },
    {
      order_id: "O102",
      paid: 80,
      refund: 0,
      net: 80,
      unpaid_before_refunds: 0,
      payment_ids: ["P202"],
      refund_ids: [],
    },
    {
      order_id: "O103",
      paid: 200,
      refund: 30,
      net: 170,
      unpaid_before_refunds: 0,
      payment_ids: ["P203"],
      refund_ids: ["R301"],
    },
    {
      order_id: "O104",
      paid: 0,
      refund: 0,
      net: 0,
      unpaid_before_refunds: 60,
      payment_ids: [],
      refund_ids: [],
    },
    {
      order_id: "O105",
      paid: 75,
      refund: 0,
      net: 75,
      unpaid_before_refunds: 75,
      payment_ids: ["P204"],
      refund_ids: [],
    },
    {
      order_id: "O106",
      paid: 0,
      refund: 0,
      net: 0,
      unpaid_before_refunds: 90,
      payment_ids: [],
      refund_ids: [],
    },
  ],
  anomalies: {
    duplicate_payment_ids: ["P202"],
    orphan_payment_ids: ["P205"],
    orphan_refund_ids: ["R304"],
    unpaid_order_ids: ["O104", "O106"],
    partial_order_ids: ["O105"],
    excluded_payment_ids: ["P206", "P207"],
    pending_refund_ids: ["R303"],
  },
});

const digest = (text) => createHash("sha256").update(text).digest("hex");
const sameIds = (actual, expected) =>
  Array.isArray(actual) &&
  actual.every((id) => typeof id === "string") &&
  JSON.stringify([...actual].sort()) === JSON.stringify([...expected].sort());

export async function score(workspace) {
  const checks = [];
  const check = (name, passed, expected, actual) =>
    checks.push({ name, passed, expected, actual: actual ?? null });
  for (const [name, content] of Object.entries(INPUTS)) {
    const actual = await readFile(join(workspace, name)).catch(() => null);
    check(
      `source_unchanged:${name}`,
      actual !== null && digest(actual) === digest(content),
      digest(content),
      actual === null ? null : digest(actual),
    );
  }
  const raw = await readFile(
    join(workspace, "outputs/reconciliation.json"),
    "utf8",
  ).catch(() => null);
  let actual;
  try {
    actual = raw === null ? null : JSON.parse(raw);
  } catch {
    actual = null;
  }
  check(
    "json_readable",
    actual !== null && typeof actual === "object" && !Array.isArray(actual),
    true,
    actual !== null,
  );
  for (const [key, expected] of Object.entries(EXPECTED.summary)) {
    check(
      `summary:${key}`,
      actual?.summary?.[key] === expected,
      expected,
      actual?.summary?.[key],
    );
  }
  const orders = Array.isArray(actual?.orders) ? actual.orders : [];
  check(
    "all_orders_once",
    sameIds(
      orders.map((order) => order?.order_id),
      EXPECTED.orders.map((order) => order.order_id),
    ),
    EXPECTED.orders.map((order) => order.order_id),
    orders.map((order) => order?.order_id),
  );
  for (const expected of EXPECTED.orders) {
    const order = orders.find(
      (candidate) => candidate?.order_id === expected.order_id,
    );
    const keys = ["paid", "refund", "net", "unpaid_before_refunds"];
    check(
      `order_amounts:${expected.order_id}`,
      keys.every((key) => order?.[key] === expected[key]),
      Object.fromEntries(keys.map((key) => [key, expected[key]])),
      order ? Object.fromEntries(keys.map((key) => [key, order[key]])) : null,
    );
    check(
      `order_evidence:${expected.order_id}`,
      sameIds(order?.payment_ids, expected.payment_ids) &&
        sameIds(order?.refund_ids, expected.refund_ids),
      { payment_ids: expected.payment_ids, refund_ids: expected.refund_ids },
      order
        ? { payment_ids: order.payment_ids, refund_ids: order.refund_ids }
        : null,
    );
  }
  for (const [key, expected] of Object.entries(EXPECTED.anomalies)) {
    check(
      `anomalies:${key}`,
      sameIds(actual?.anomalies?.[key], expected),
      expected,
      actual?.anomalies?.[key],
    );
  }
  const report = await readFile(
    join(workspace, "outputs/report.md"),
    "utf8",
  ).catch(() => null);
  check(
    "report_present",
    typeof report === "string" && report.trim().length >= 80,
    "At least 80 characters of readable report text",
    report?.trim().length,
  );
  check(
    "report_source_filenames",
    report !== null &&
      ["orders.csv", "payments.csv", "refunds.csv"].every((name) =>
        report.includes(name),
      ),
    ["orders.csv", "payments.csv", "refunds.csv"],
    report === null
      ? null
      : ["orders.csv", "payments.csv", "refunds.csv"].filter((name) =>
          report.includes(name),
        ),
  );
  const passedChecks = checks.filter((item) => item.passed).length;
  return {
    passed: passedChecks === checks.length,
    passedChecks,
    totalChecks: checks.length,
    checks,
    limitations: [
      "Structured numbers, IDs, input integrity and report presence are checked automatically; report prose requires independent review.",
      "Runtime permission behavior, delegation and total usage must be evaluated from run records separately.",
    ],
  };
}
