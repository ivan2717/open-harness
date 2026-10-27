#!/usr/bin/env python3
"""Auditable, read-only reconciliation of the four specified source files."""

import csv
from collections import Counter, defaultdict
from decimal import Decimal
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def read_csv(filename):
    with (ROOT / filename).open("r", encoding="utf-8-sig", newline="") as stream:
        return list(csv.DictReader(stream))


def money(value):
    return Decimal(value)


def fmt(value):
    return f"{value:.2f}"


orders_rows = read_csv("orders.csv")
payment_rows = read_csv("payments.csv")
refund_rows = read_csv("refunds.csv")

orders = {row["order_id"]: money(row["amount"]) for row in orders_rows}
assert len(orders) == len(orders_rows), "orders.csv has duplicate order IDs"

# Deduplicate strictly by payment_id, checking exact row agreement.
payment_by_id = {}
payment_counts = Counter()
for row in payment_rows:
    payment_id = row["payment_id"]
    payment_counts[payment_id] += 1
    if payment_id in payment_by_id:
        assert payment_by_id[payment_id] == row, f"Conflicting duplicate {payment_id}"
    else:
        payment_by_id[payment_id] = row
duplicate_ids = sorted(pid for pid, count in payment_counts.items() if count > 1)

valid_payments = {}
failed_ids = []
cancelled_ids = []
orphan_payment_ids = []
for payment_id, row in payment_by_id.items():
    if row["status"] == "paid":
        if row["order_id"] in orders:
            valid_payments[payment_id] = row
        else:
            orphan_payment_ids.append(payment_id)
    elif row["status"] == "failed":
        failed_ids.append(payment_id)
    elif row["status"] == "cancelled":
        cancelled_ids.append(payment_id)

valid_by_order = defaultdict(Decimal)
for payment_id, row in valid_payments.items():
    valid_by_order[row["order_id"]] += money(row["amount"])

payment_order = {pid: row["order_id"] for pid, row in valid_payments.items()}
refund_effects = defaultdict(Decimal)
included_refunds = []
pending_excluded = []
orphan_refunds = []
refund_decisions = []

for row in refund_rows:
    refund_id = row["refund_id"]
    status = row["status"]
    effective_status = status
    reason = ""
    if refund_id == "R303":
        effective_status = "settled"
        reason = 'user correction CORRECTION-ORCHID (event-4): “R303 已经人工核实为 settled。”'

    if effective_status != "settled":
        pending_excluded.append(refund_id)
        refund_decisions.append(
            f"{refund_id}: EXCLUDED (source status={status}; not settled)"
        )
        continue

    payment_id = row["payment_id"]
    if payment_id not in valid_payments:
        orphan_refunds.append(refund_id)
        refund_decisions.append(
            f"{refund_id}: ORPHAN (settled/effectively settled, payment {payment_id} "
            "is not an included valid payment)"
        )
        continue

    if row["order_id"] != payment_order[payment_id]:
        orphan_refunds.append(refund_id)
        refund_decisions.append(
            f"{refund_id}: ORPHAN (refund order {row['order_id']} does not match "
            f"payment {payment_id} order {payment_order[payment_id]})"
        )
        continue

    included_refunds.append(refund_id)
    refund_effects[row["order_id"]] += money(row["amount"])
    refund_decisions.append(
        f"{refund_id}: INCLUDED amount={fmt(money(row['amount']))} "
        f"order={row['order_id']} payment={payment_id}; {reason or 'source status=settled'}"
    )

order_net = {
    order_id: valid_by_order[order_id] - refund_effects[order_id]
    for order_id in orders
}
unpaid_by_order = {
    order_id: max(orders[order_id] - valid_by_order[order_id], Decimal(0))
    for order_id in orders
}
unpaid_orders = [oid for oid in orders if valid_by_order[oid] == 0]
partial_orders = [
    oid for oid in orders if Decimal(0) < valid_by_order[oid] < orders[oid]
]
order_total = sum(orders.values(), Decimal(0))
valid_payment_total = sum(
    (money(row["amount"]) for row in valid_payments.values()), Decimal(0)
)
applied_refund = sum(refund_effects.values(), Decimal(0))
net_received = sum(order_net.values(), Decimal(0))
unpaid_before_refunds = sum(unpaid_by_order.values(), Decimal(0))

print("ORDER RECONCILIATION — AUDIT SUMMARY")
print("Inputs read: orders.csv, payments.csv, refunds.csv, rules.md")
print("Rules source: rules.md; amounts in yuan; one settlement period.")
print('Applied user correction from parent-session event-4: CORRECTION-ORCHID, “R303 已经人工核实为 settled。”')
print("\nORDERS")
print(f"Unique order count: {len(orders)}; total ordered={fmt(order_total)}")
for order_id, amount in orders.items():
    print(f"  {order_id}: amount={fmt(amount)}")

print("\nPAYMENTS")
print(f"Payment rows={len(payment_rows)}; unique payment IDs={len(payment_by_id)}")
print(f"Duplicate payment IDs (identical repeated rows counted once): {duplicate_ids}")
print(f"Included valid paid payments ({len(valid_payments)}):")
for payment_id, row in valid_payments.items():
    print(f"  {payment_id}: order={row['order_id']} amount={fmt(money(row['amount']))}")
print(f"Valid payment total={fmt(valid_payment_total)}")
print(f"Excluded failed payment IDs: {sorted(failed_ids)}")
print(f"Excluded cancelled payment IDs: {sorted(cancelled_ids)}")
print(f"Orphan paid payment IDs (unknown order): {sorted(orphan_payment_ids)}")

print("\nREFUNDS")
print(f"Refund source rows={len(refund_rows)}")
for decision in refund_decisions:
    print(f"  {decision}")
print(f"Included refund IDs: {included_refunds}")
print(f"Excluded pending refund IDs (source status pending, excluding override): {pending_excluded}")
print(f"Orphan settled/effectively settled refund IDs: {orphan_refunds}")
print(f"Applied refund total={fmt(applied_refund)}")

print("\nPER-ORDER CALCULATION")
for order_id in orders:
    print(
        f"  {order_id}: order={fmt(orders[order_id])} - valid_payments="
        f"{fmt(valid_by_order[order_id])} - applied_refunds="
        f"{fmt(refund_effects[order_id])} = net_received={fmt(order_net[order_id])}; "
        f"unpaid_before_refunds=max(order-valid_payments,0)="
        f"{fmt(unpaid_by_order[order_id])}"
    )

print("\nTOTALS")
print(f"Net received = valid payments {fmt(valid_payment_total)} - applied refunds {fmt(applied_refund)} = {fmt(net_received)}")
print(f"unpaid_before_refunds = {fmt(unpaid_before_refunds)}")
print(f"Unpaid orders (valid payment=0): {unpaid_orders}")
print(f"Partial payment orders: {partial_orders}")
print(f"Refund IDs counted: {included_refunds}")
