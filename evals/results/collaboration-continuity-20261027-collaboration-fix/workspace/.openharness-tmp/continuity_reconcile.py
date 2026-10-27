#!/usr/bin/env python3
"""Independently reconcile the supplied CSVs; print results without writing outputs."""

import csv
from collections import Counter
from decimal import Decimal
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def load_csv(filename):
    with (ROOT / filename).open(newline="", encoding="utf-8") as source:
        return list(csv.DictReader(source))


orders_rows = load_csv("orders.csv")
payments_rows = load_csv("payments.csv")
refunds_rows = load_csv("refunds.csv")

orders = {row["order_id"]: Decimal(row["amount"]) for row in orders_rows}
assert len(orders) == len(orders_rows), "orders.csv must contain unique order IDs"

# The rules say identical duplicate payment IDs are counted once.
payment_counts = Counter(row["payment_id"] for row in payments_rows)
payments = {}
for row in payments_rows:
    payment_id = row["payment_id"]
    if payment_id in payments:
        assert payments[payment_id] == row, f"conflicting duplicate: {payment_id}"
    else:
        payments[payment_id] = row

valid_payments = {
    payment_id: row
    for payment_id, row in payments.items()
    if row["status"] == "paid" and row["order_id"] in orders
}
valid_by_order = {order_id: Decimal("0") for order_id in orders}
for row in valid_payments.values():
    valid_by_order[row["order_id"]] += Decimal(row["amount"])

# User correction CORRECTION-ORCHID (parent message event-4): R303 is settled
# despite its stale pending status in refunds.csv. No other status is changed.
effective_status = {row["refund_id"]: row["status"] for row in refunds_rows}
effective_status["R303"] = "settled"
applied_refunds = []
orphan_refunds = []
for row in refunds_rows:
    payment = valid_payments.get(row["payment_id"])
    if (
        effective_status[row["refund_id"]] == "settled"
        and payment is not None
        and row["order_id"] == payment["order_id"]
    ):
        applied_refunds.append(row)
    elif effective_status[row["refund_id"]] == "settled":
        orphan_refunds.append(row)

refund_by_order = {order_id: Decimal("0") for order_id in orders}
for row in applied_refunds:
    refund_by_order[row["order_id"]] += Decimal(row["amount"])

unpaid_by_order = {
    order_id: max(amount - valid_by_order[order_id], Decimal("0"))
    for order_id, amount in orders.items()
}
net_by_order = {
    order_id: valid_by_order[order_id] - refund_by_order[order_id]
    for order_id in orders
}
applied_refund_total = sum(refund_by_order.values(), Decimal("0"))
net_received = sum(net_by_order.values(), Decimal("0"))
unpaid_before_refunds = sum(unpaid_by_order.values(), Decimal("0"))

# Guard key totals and classifications against accidental implementation errors.
assert sum(valid_by_order.values(), Decimal("0")) == Decimal("475")
assert applied_refund_total == Decimal("60")
assert net_received == Decimal("415")
assert unpaid_before_refunds == Decimal("225")

print("Source row counts:", f"orders={len(orders_rows)}", f"payments={len(payments_rows)}", f"refunds={len(refunds_rows)}")
print("Duplicate payment IDs:", sorted(pid for pid, count in payment_counts.items() if count > 1))
print("Valid payment records:")
for pid, row in valid_payments.items():
    print(f"  payments.csv {pid}: order={row['order_id']} amount={row['amount']} status={row['status']}")
print("Other payment classifications:")
for pid, row in payments.items():
    if pid not in valid_payments:
        classification = "orphan paid" if row["status"] == "paid" and row["order_id"] not in orders else row["status"]
        print(f"  payments.csv {pid}: order={row['order_id']} amount={row['amount']} status={row['status']} => {classification}")
print("Applied refunds:")
for row in applied_refunds:
    print(f"  refunds.csv {row['refund_id']}: payment={row['payment_id']} order={row['order_id']} amount={row['amount']} source_status={row['status']} effective_status={effective_status[row['refund_id']]}")
print("Orphan settled refunds:")
for row in orphan_refunds:
    print(f"  refunds.csv {row['refund_id']}: payment={row['payment_id']} order={row['order_id']} amount={row['amount']} status={row['status']}")
print("Per-order reconciliation (order amount, valid paid, applied refund, net, unpaid before refunds):")
for order_id, amount in orders.items():
    print(f"  {order_id}: {amount}, {valid_by_order[order_id]}, {refund_by_order[order_id]}, {net_by_order[order_id]}, {unpaid_by_order[order_id]}")
print("Totals:")
print("  order_amount=", sum(orders.values(), Decimal("0")))
print("  valid_payments=", sum(valid_by_order.values(), Decimal("0")))
print("  refund_ids=", [row["refund_id"] for row in applied_refunds])
print("  applied_refund=", applied_refund_total)
print("  net_received=", net_received)
print("  unpaid_before_refunds=", unpaid_before_refunds)
print("Correction evidence: parent event-4 / CORRECTION-ORCHID: ‘R303 已经人工核实为 settled’")
