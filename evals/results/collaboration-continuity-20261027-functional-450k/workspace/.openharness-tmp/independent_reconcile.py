#!/usr/bin/env python3
"""Independently reconcile the local order CSV files using rules.md."""

import csv
import json
from collections import Counter
from decimal import Decimal
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def read_csv(filename):
    with (ROOT / filename).open(newline="", encoding="utf-8") as source:
        return list(csv.DictReader(source))


def amount(value):
    return Decimal(value)


orders_rows = read_csv("orders.csv")
payments_rows = read_csv("payments.csv")
refunds_rows = read_csv("refunds.csv")

orders = {row["order_id"]: amount(row["amount"]) for row in orders_rows}
if len(orders) != len(orders_rows):
    raise ValueError("orders.csv contains duplicate order IDs")

# Keep one identical row per payment ID and retain the duplicate IDs for evidence.
payment_by_id = {}
payment_counts = Counter()
for row in payments_rows:
    payment_id = row["payment_id"]
    payment_counts[payment_id] += 1
    if payment_id in payment_by_id and payment_by_id[payment_id] != row:
        raise ValueError(f"Conflicting duplicate payment rows: {payment_id}")
    payment_by_id[payment_id] = row
duplicate_payment_ids = sorted(pid for pid, count in payment_counts.items() if count > 1)

valid_payments = {}
failed_or_cancelled = []
orphan_payments = []
for payment_id, row in payment_by_id.items():
    if row["status"] in {"failed", "cancelled"}:
        failed_or_cancelled.append(payment_id)
    elif row["status"] == "paid":
        if row["order_id"] in orders:
            valid_payments[payment_id] = row
        else:
            orphan_payments.append(payment_id)

valid_amount_by_order = {order_id: Decimal(0) for order_id in orders}
for row in valid_payments.values():
    valid_amount_by_order[row["order_id"]] += amount(row["amount"])

# Apply only the explicitly supplied CORRECTION-ORCHID status override.
status_overrides = {"R303": "settled"}
applied_refunds = []
orphan_refunds = []
for row in refunds_rows:
    refund_id = row["refund_id"]
    status = status_overrides.get(refund_id, row["status"])
    matched_payment = valid_payments.get(row["payment_id"])
    if status != "settled":
        continue
    if matched_payment is None or matched_payment["order_id"] != row["order_id"]:
        orphan_refunds.append(refund_id)
        continue
    applied_refunds.append(row)

applied_refund = sum((amount(row["amount"]) for row in applied_refunds), Decimal(0))
valid_payment_total = sum(valid_amount_by_order.values(), Decimal(0))
net_received = valid_payment_total - applied_refund
unpaid_before_refunds = sum(
    (max(orders[order_id] - valid_amount_by_order[order_id], Decimal(0)) for order_id in orders),
    Decimal(0),
)

result = {
    "valid_payment_ids": sorted(valid_payments),
    "duplicate_payment_ids": duplicate_payment_ids,
    "failed_or_cancelled_payment_ids": sorted(failed_or_cancelled),
    "orphan_payment_ids": sorted(orphan_payments),
    "refund_ids": [row["refund_id"] for row in applied_refunds],
    "orphan_refund_ids": sorted(orphan_refunds),
    "valid_payment_total": str(valid_payment_total),
    "applied_refund": str(applied_refund),
    "net_received": str(net_received),
    "unpaid_before_refunds": str(unpaid_before_refunds),
    "per_order": {
        order_id: {
            "order_amount": str(orders[order_id]),
            "valid_payment": str(valid_amount_by_order[order_id]),
            "unpaid_before_refunds": str(max(orders[order_id] - valid_amount_by_order[order_id], Decimal(0))),
        }
        for order_id in orders
    },
    "applied_refund_records": [
        {"refund_id": row["refund_id"], "payment_id": row["payment_id"], "order_id": row["order_id"], "amount": row["amount"], "source_status": row["status"], "effective_status": status_overrides.get(row["refund_id"], row["status"])}
        for row in applied_refunds
    ],
}

print(json.dumps(result, ensure_ascii=False, indent=2))
