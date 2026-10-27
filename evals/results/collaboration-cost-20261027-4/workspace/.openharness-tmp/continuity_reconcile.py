#!/usr/bin/env python3
"""Independently recompute the requested reconciliation from the four CSV/rules files.

Prints a report to stdout only; it does not alter source files or create deliverables.
"""

import csv
import json
from collections import Counter, defaultdict
from decimal import Decimal
from pathlib import Path


def read_csv(filename):
    with Path(filename).open(newline="", encoding="utf-8") as stream:
        return list(csv.DictReader(stream))


orders_rows = read_csv("orders.csv")
payment_rows = read_csv("payments.csv")
refund_rows = read_csv("refunds.csv")

orders = {}
for row in orders_rows:
    order_id = row["order_id"]
    if order_id in orders:
        raise ValueError(f"duplicate order_id: {order_id}")
    orders[order_id] = Decimal(row["amount"])

payment_counts = Counter(row["payment_id"] for row in payment_rows)
payments = {}
for row in payment_rows:
    payment_id = row["payment_id"]
    canonical = {key: row[key] for key in ("order_id", "amount", "status")}
    if payment_id in payments and payments[payment_id] != canonical:
        raise ValueError(f"inconsistent duplicate payment_id: {payment_id}")
    payments[payment_id] = canonical

valid_payments = {}
noncounted_status_ids = defaultdict(list)
orphan_payment_ids = []
for payment_id, payment in payments.items():
    if payment["status"] != "paid":
        noncounted_status_ids[payment["status"]].append(payment_id)
    elif payment["order_id"] not in orders:
        orphan_payment_ids.append(payment_id)
    else:
        valid_payments[payment_id] = payment

refund_ids = []
applied_refund = Decimal("0")
orphan_refund_ids = []
excluded_pending_ids = []
net_by_order = {order_id: Decimal("0") for order_id in orders}
paid_by_order = {order_id: Decimal("0") for order_id in orders}
for payment in valid_payments.values():
    paid_by_order[payment["order_id"]] += Decimal(payment["amount"])
    net_by_order[payment["order_id"]] += Decimal(payment["amount"])

for refund in refund_rows:
    refund_id = refund["refund_id"]
    if refund_id == "R303":
        # User correction CORRECTION-ORCHID (event-4) overrides the stale CSV status.
        effective_status = "settled"
    else:
        effective_status = refund["status"]
    if effective_status == "pending":
        excluded_pending_ids.append(refund_id)
        continue
    if effective_status != "settled":
        continue
    payment = valid_payments.get(refund["payment_id"])
    if payment is None or refund["order_id"] != payment["order_id"]:
        orphan_refund_ids.append(refund_id)
        continue
    amount = Decimal(refund["amount"])
    refund_ids.append(refund_id)
    applied_refund += amount
    net_by_order[refund["order_id"]] -= amount

unpaid_before_refunds = sum(
    max(orders[order_id] - paid_by_order[order_id], Decimal("0"))
    for order_id in orders
)
net_received = sum(net_by_order.values())

report = {
    "orders": {key: str(value) for key, value in orders.items()},
    "duplicate_payment_ids": sorted(
        payment_id for payment_id, count in payment_counts.items() if count > 1
    ),
    "valid_payment_ids": sorted(valid_payments),
    "valid_payments_by_order": {
        order_id: str(amount) for order_id, amount in paid_by_order.items()
    },
    "failed_payment_ids": sorted(noncounted_status_ids["failed"]),
    "cancelled_payment_ids": sorted(noncounted_status_ids["cancelled"]),
    "orphan_payment_ids": sorted(orphan_payment_ids),
    "refund_ids": refund_ids,
    "applied_refund": str(applied_refund),
    "orphan_refund_ids": orphan_refund_ids,
    "excluded_pending_ids": excluded_pending_ids,
    "unpaid_before_refunds_by_order": {
        order_id: str(max(orders[order_id] - paid_by_order[order_id], Decimal("0")))
        for order_id in orders
    },
    "unpaid_before_refunds": str(unpaid_before_refunds),
    "net_by_order": {key: str(value) for key, value in net_by_order.items()},
    "net_received": str(net_received),
    "total_order_amount": str(sum(orders.values())),
    "valid_payment_total": str(sum(paid_by_order.values())),
}
print(json.dumps(report, indent=2, ensure_ascii=False))
