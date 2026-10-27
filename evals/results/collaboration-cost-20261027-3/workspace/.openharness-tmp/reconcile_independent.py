#!/usr/bin/env python3
"""Independently reconcile the four supplied order records, without writes to sources."""

import csv
from collections import Counter
from decimal import Decimal
from pathlib import Path


def read_csv(name):
    with Path(name).open("r", encoding="utf-8", newline="") as stream:
        return list(csv.DictReader(stream))


orders = {row["order_id"]: Decimal(row["amount"]) for row in read_csv("orders.csv")}
payment_rows = read_csv("payments.csv")
refund_rows = read_csv("refunds.csv")

# The rule says duplicate payment IDs have identical content and count once.
payment_counts = Counter(row["payment_id"] for row in payment_rows)
payments = {}
for row in payment_rows:
    payments.setdefault(row["payment_id"], row)

valid_payments = {}
nonpaid_ids = []
orphan_payment_ids = []
for payment_id, row in payments.items():
    if row["status"] != "paid":
        nonpaid_ids.append(payment_id)
    elif row["order_id"] not in orders:
        orphan_payment_ids.append(payment_id)
    else:
        valid_payments[payment_id] = row

applied_refunds = []
orphan_refund_ids = []
excluded_refunds = []
refund_by_order = {order_id: Decimal("0") for order_id in orders}
refund_override = {"R303": "settled"}  # User correction CORRECTION-ORCHID.
for row in refund_rows:
    refund_id = row["refund_id"]
    status = refund_override.get(refund_id, row["status"])
    payment = valid_payments.get(row["payment_id"])
    if status == "settled" and payment and row["order_id"] == payment["order_id"]:
        applied_refunds.append(refund_id)
        refund_by_order[row["order_id"]] += Decimal(row["amount"])
    elif status == "settled":
        orphan_refund_ids.append(refund_id)
    else:
        excluded_refunds.append(refund_id)

paid_by_order = {order_id: Decimal("0") for order_id in orders}
for row in valid_payments.values():
    paid_by_order[row["order_id"]] += Decimal(row["amount"])

net_by_order = {
    order_id: paid_by_order[order_id] - refund_by_order[order_id]
    for order_id in orders
}
unpaid_before_refunds = sum(
    (max(amount - paid_by_order[order_id], Decimal("0")) for order_id, amount in orders.items()),
    Decimal("0"),
)
order_total = sum(orders.values(), Decimal("0"))
valid_payment_total = sum(paid_by_order.values(), Decimal("0"))
applied_refund_total = sum(refund_by_order.values(), Decimal("0"))
net_received = sum(net_by_order.values(), Decimal("0"))

print("Source records:")
for order_id, amount in orders.items():
    print(f"  orders.csv {order_id}: amount={amount}")
for payment_id, row in payments.items():
    duplicate_note = f" duplicate_rows={payment_counts[payment_id] - 1}" if payment_counts[payment_id] > 1 else ""
    classification = (
        "valid" if payment_id in valid_payments
        else "orphan_paid" if payment_id in orphan_payment_ids
        else f"excluded_{row['status']}"
    )
    print(f"  payments.csv {payment_id}: order={row['order_id']} amount={row['amount']} status={row['status']} classification={classification}{duplicate_note}")
for row in refund_rows:
    refund_id = row["refund_id"]
    effective_status = refund_override.get(refund_id, row["status"])
    classification = (
        "applied" if refund_id in applied_refunds
        else "orphan_settled" if refund_id in orphan_refund_ids
        else "excluded_pending"
    )
    correction = " correction=CORRECTION-ORCHID(event-4)" if refund_id in refund_override else ""
    payment = valid_payments.get(row["payment_id"])
    match = "matches" if payment and row["order_id"] == payment["order_id"] else "no_valid_matching_payment/order"
    print(f"  refunds.csv {refund_id}: payment={row['payment_id']} order={row['order_id']} amount={row['amount']} source_status={row['status']} effective_status={effective_status} match={match} classification={classification}{correction}")

print("Payment deduplication:")
print(f"  repeated payment IDs: {[key for key, count in payment_counts.items() if count > 1]}")
print(f"  non-paid IDs: {sorted(nonpaid_ids)}")
print(f"  paid orphan IDs: {sorted(orphan_payment_ids)}")
print("Totals:")
print(f"  orders_total={order_total}")
print(f"  valid_payment_total={valid_payment_total}")
print(f"  applied_refund_ids={applied_refunds}")
print(f"  applied_refund={applied_refund_total}")
print(f"  orphan_refund_ids={orphan_refund_ids}")
print(f"  excluded_refund_ids={excluded_refunds}")
print(f"  net_received={net_received}")
print(f"  unpaid_before_refunds={unpaid_before_refunds}")
print("Per-order calculation (amount, valid paid, applied refund, net, unpaid before refunds):")
for order_id, amount in orders.items():
    unpaid = max(amount - paid_by_order[order_id], Decimal("0"))
    print(f"  {order_id}: {amount}, {paid_by_order[order_id]}, {refund_by_order[order_id]}, {net_by_order[order_id]}, {unpaid}")
print(f"  unpaid_orders={[order_id for order_id in orders if paid_by_order[order_id] == 0]}")
print(f"  partially_paid_orders={[order_id for order_id in orders if 0 < paid_by_order[order_id] < orders[order_id]]}")

# Guard against accidental or unannounced source-data changes during the run.
assert order_total == Decimal("700")
assert valid_payment_total == Decimal("475")
assert applied_refunds == ["R301", "R302", "R303"]
assert applied_refund_total == Decimal("60")
assert orphan_refund_ids == ["R304"]
assert excluded_refunds == []
assert net_received == Decimal("415")
assert unpaid_before_refunds == Decimal("225")
print("VERIFICATION: all reconciliation assertions passed")
