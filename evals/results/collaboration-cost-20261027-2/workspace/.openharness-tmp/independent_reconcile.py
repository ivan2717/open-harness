import csv
import json
from collections import Counter
from decimal import Decimal
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read_csv(name):
    with (ROOT / name).open(newline="", encoding="utf-8") as stream:
        return list(csv.DictReader(stream))


orders_rows = read_csv("orders.csv")
payments_rows = read_csv("payments.csv")
refunds_rows = read_csv("refunds.csv")

orders = {row["order_id"]: Decimal(row["amount"]) for row in orders_rows}

# Payment IDs are deduplicated before eligibility checks, per rules.md.
payment_counts = Counter(row["payment_id"] for row in payments_rows)
payments_by_id = {}
for row in payments_rows:
    payments_by_id.setdefault(row["payment_id"], row)
duplicates = sorted(payment_id for payment_id, count in payment_counts.items() if count > 1)

valid_payments = {}
failed_or_cancelled = []
orphan_payments = []
for payment_id, row in payments_by_id.items():
    if row["status"] in ("failed", "cancelled"):
        failed_or_cancelled.append(payment_id)
    elif row["status"] == "paid" and row["order_id"] not in orders:
        orphan_payments.append(payment_id)
    elif row["status"] == "paid":
        valid_payments[payment_id] = row

paid_by_order = {order_id: Decimal("0") for order_id in orders}
for row in valid_payments.values():
    paid_by_order[row["order_id"]] += Decimal(row["amount"])

# The settled user correction applies solely to R303 (parent event-4).
corrected_settled = {"R303"}
applied_refunds = []
orphan_refunds = []
pending_refunds = []
for row in refunds_rows:
    settled = row["status"] == "settled" or row["refund_id"] in corrected_settled
    payment = valid_payments.get(row["payment_id"])
    if settled and payment is not None and payment["order_id"] == row["order_id"]:
        applied_refunds.append(row)
    elif settled:
        orphan_refunds.append(row["refund_id"])
    else:
        pending_refunds.append(row["refund_id"])

refund_by_order = {order_id: Decimal("0") for order_id in orders}
for row in applied_refunds:
    refund_by_order[row["order_id"]] += Decimal(row["amount"])

order_net = {
    order_id: paid_by_order[order_id] - refund_by_order[order_id]
    for order_id in orders
}
applied_refund = sum((Decimal(row["amount"]) for row in applied_refunds), Decimal("0"))
net_received = sum(order_net.values(), Decimal("0"))
unpaid_before_refunds = sum(
    (max(orders[order_id] - paid_by_order[order_id], Decimal("0")) for order_id in orders),
    Decimal("0"),
)


def number(value):
    return int(value) if value == value.to_integral_value() else float(value)


result = {
    "order_total": number(sum(orders.values(), Decimal("0"))),
    "valid_payment_ids": sorted(valid_payments),
    "valid_payment_amount": number(sum(paid_by_order.values(), Decimal("0"))),
    "duplicate_payment_ids": duplicates,
    "excluded_failed_cancelled_payment_ids": sorted(failed_or_cancelled),
    "orphan_payment_ids": sorted(orphan_payments),
    "applied_refund_ids": [row["refund_id"] for row in applied_refunds],
    "applied_refund": number(applied_refund),
    "orphan_refund_ids": sorted(orphan_refunds),
    "pending_refund_ids": sorted(pending_refunds),
    "net_received": number(net_received),
    "unpaid_before_refunds": number(unpaid_before_refunds),
    "order_net": {key: number(value) for key, value in order_net.items()},
    "unpaid_orders": sorted(
        order_id for order_id, amount in paid_by_order.items() if amount == 0
    ),
    "partially_paid_orders": sorted(
        order_id
        for order_id, amount in paid_by_order.items()
        if 0 < amount < orders[order_id]
    ),
}
print(json.dumps(result, ensure_ascii=False, indent=2))
