#!/usr/bin/env python3
"""Reconcile the supplied order records without modifying source files."""

import csv
import json
from collections import Counter
from decimal import Decimal
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CORRECTED_REFUND_ID = "R303"
CORRECTION_EVIDENCE = {
    "message_id": "event-4",
    "correction_id": "CORRECTION-ORCHID",
    "quote": "R303 已经人工核实为 settled",
}


def read_csv(name):
    with (ROOT / name).open("r", encoding="utf-8-sig", newline="") as source:
        return list(csv.DictReader(source))


def money(value):
    return Decimal(value)


orders_rows = read_csv("orders.csv")
payments_rows = read_csv("payments.csv")
refunds = read_csv("refunds.csv")
orders = {row["order_id"]: money(row["amount"]) for row in orders_rows}
if len(orders) != len(orders_rows):
    raise ValueError("orders.csv contains duplicate order IDs")

# Keep first row per payment_id, as instructed; verify duplicates agree.
payment_groups = {}
for row in payments_rows:
    payment_groups.setdefault(row["payment_id"], []).append(row)
payments = {}
for payment_id, rows in payment_groups.items():
    if any(row != rows[0] for row in rows[1:]):
        raise ValueError(f"Conflicting duplicate payment rows: {payment_id}")
    payments[payment_id] = rows[0]

effective_payments = {}
excluded_status_payments = []
orphan_payments = []
for payment_id, row in payments.items():
    if row["status"] != "paid":
        excluded_status_payments.append(payment_id)
    elif row["order_id"] not in orders:
        orphan_payments.append(payment_id)
    else:
        effective_payments[payment_id] = row

paid_by_order = {order_id: Decimal("0") for order_id in orders}
for row in effective_payments.values():
    paid_by_order[row["order_id"]] += money(row["amount"])

refund_ids = []
applied_refund = Decimal("0")
orphan_refunds = []
for refund in refunds:
    corrected_to_settled = refund["refund_id"] == CORRECTED_REFUND_ID
    status_is_settled = refund["status"] == "settled" or corrected_to_settled
    payment = effective_payments.get(refund["payment_id"])
    matches = payment is not None and payment["order_id"] == refund["order_id"]
    if status_is_settled and matches:
        refund_ids.append(refund["refund_id"])
        applied_refund += money(refund["amount"])
    elif refund["status"] == "settled" and not matches:
        orphan_refunds.append(refund["refund_id"])

unpaid_by_order = {
    order_id: max(amount - paid_by_order[order_id], Decimal("0"))
    for order_id, amount in orders.items()
}
unpaid_before_refunds = sum(unpaid_by_order.values(), Decimal("0"))
effective_payment_total = sum(
    (money(row["amount"]) for row in effective_payments.values()), Decimal("0")
)
net_received = effective_payment_total - applied_refund


def output_decimal(value):
    return int(value) if value == value.to_integral_value() else float(value)


report = {
    "orders": [
        {"order_id": order_id, "amount": output_decimal(amount)}
        for order_id, amount in orders.items()
    ],
    "duplicate_payment_ids": [
        payment_id for payment_id, rows in payment_groups.items() if len(rows) > 1
    ],
    "effective_payments": [
        {"payment_id": payment_id, "order_id": row["order_id"], "amount": output_decimal(money(row["amount"]))}
        for payment_id, row in effective_payments.items()
    ],
    "excluded_status_payment_ids": excluded_status_payments,
    "orphan_payment_ids": orphan_payments,
    "refunds_applied": [
        {
            "refund_id": refund_id,
            "amount": output_decimal(money(next(r["amount"] for r in refunds if r["refund_id"] == refund_id))),
            "status_in_source": next(r["status"] for r in refunds if r["refund_id"] == refund_id),
            "manual_status_correction": refund_id == CORRECTED_REFUND_ID,
        }
        for refund_id in refund_ids
    ],
    "orphan_refund_ids": orphan_refunds,
    "effective_payment_total": output_decimal(effective_payment_total),
    "refund_ids": refund_ids,
    "applied_refund": output_decimal(applied_refund),
    "net_received": output_decimal(net_received),
    "unpaid_before_refunds_by_order": {
        order_id: output_decimal(amount) for order_id, amount in unpaid_by_order.items()
    },
    "unpaid_before_refunds": output_decimal(unpaid_before_refunds),
    "evidence": [
        "orders.csv: O101–O106 and amounts; rules.md rules 1 and 7.",
        "payments.csv: P201–P207; P202 occurs twice with identical contents (deduplicated); rules.md rules 2–4.",
        "refunds.csv: R301–R304; R301/R302 are settled; R303 is pending in source; R304 references unmatched P999; rules.md rule 5.",
        f"User correction {CORRECTION_EVIDENCE['correction_id']} ({CORRECTION_EVIDENCE['message_id']}): “{CORRECTION_EVIDENCE['quote']}”; applies only to R303.",
    ],
}
print(json.dumps(report, ensure_ascii=False, indent=2))
