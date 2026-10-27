import csv
from collections import Counter, defaultdict
from decimal import Decimal
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
MONEY = Decimal


def read_csv(name):
    with (ROOT / name).open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


orders_rows = read_csv("orders.csv")
payment_rows = read_csv("payments.csv")
refund_rows = read_csv("refunds.csv")
rules_text = (ROOT / "rules.md").read_text(encoding="utf-8")

orders = {row["order_id"]: MONEY(row["amount"]) for row in orders_rows}
assert len(orders) == len(orders_rows), "orders.csv contains duplicate order IDs"

# Rules: deduplicate by payment_id before evaluating status/order matching.
payment_occurrences = Counter(row["payment_id"] for row in payment_rows)
deduped_payments = {}
for row in payment_rows:
    deduped_payments.setdefault(row["payment_id"], row)
for payment_id in deduped_payments:
    rows = [r for r in payment_rows if r["payment_id"] == payment_id]
    assert len({tuple(sorted(r.items())) for r in rows}) == 1, (
        f"conflicting duplicate payment rows: {payment_id}"
    )

valid_payments = {}
invalid_status = []
orphan_payments = []
for payment_id, row in deduped_payments.items():
    if row["status"] != "paid":
        invalid_status.append(payment_id)
    elif row["order_id"] not in orders:
        orphan_payments.append(payment_id)
    else:
        valid_payments[payment_id] = {
            "order_id": row["order_id"],
            "amount": MONEY(row["amount"]),
        }

valid_payment_by_order = defaultdict(lambda: MONEY("0"))
for payment in valid_payments.values():
    valid_payment_by_order[payment["order_id"]] += payment["amount"]

# R303 is pending in the source but CORRECTION-ORCHID (parent event-4)
# explicitly overrides it to settled. No source file is changed.
settled_refund_ids = []
orphan_refunds = []
refund_total_by_order = defaultdict(lambda: MONEY("0"))
for row in refund_rows:
    status = "settled" if row["refund_id"] == "R303" else row["status"]
    if status != "settled":
        continue
    payment = valid_payments.get(row["payment_id"])
    if payment is None or payment["order_id"] != row["order_id"]:
        orphan_refunds.append(row["refund_id"])
        continue
    refund = MONEY(row["amount"])
    settled_refund_ids.append(row["refund_id"])
    refund_total_by_order[row["order_id"]] += refund

applied_refund = sum(refund_total_by_order.values(), MONEY("0"))
gross_received = sum(valid_payment_by_order.values(), MONEY("0"))
net_received = gross_received - applied_refund
unpaid_by_order = {
    order_id: max(amount - valid_payment_by_order[order_id], MONEY("0"))
    for order_id, amount in orders.items()
}
unpaid_before_refunds = sum(unpaid_by_order.values(), MONEY("0"))
net_by_order = {
    order_id: valid_payment_by_order[order_id] - refund_total_by_order[order_id]
    for order_id in orders
}

# Verify the output indicators against the per-order calculation and rules.
assert net_received == sum(net_by_order.values(), MONEY("0"))
assert "pending 不冲减" in rules_text
assert set(settled_refund_ids) == {"R301", "R302", "R303"}
assert set(orphan_payments) == {"P205"}
assert set(orphan_refunds) == {"R304"}


def fmt(value):
    return format(value, "f")


print("SOURCE COUNTS")
print(f"orders={len(orders_rows)} payment_rows={len(payment_rows)} distinct_payments={len(deduped_payments)} refunds={len(refund_rows)}")
print("PAYMENT DEDUPLICATION")
print(f"duplicate_ids={[pid for pid, count in payment_occurrences.items() if count > 1]}")
print(f"valid_paid={[(pid, p['order_id'], fmt(p['amount'])) for pid, p in valid_payments.items()]}")
print(f"excluded_status_payment_ids={sorted(invalid_status)} orphan_payment_ids={sorted(orphan_payments)}")
print("REFUNDS")
print(f"applied_refund_ids={settled_refund_ids} (R303 source status pending overridden by CORRECTION-ORCHID / parent event-4)")
print(f"orphan_refund_ids={sorted(orphan_refunds)}")
print(f"refund_by_order={{{', '.join(f'{k}: {fmt(v)}' for k, v in refund_total_by_order.items())}}}")
print("PER-ORDER RECONCILIATION")
for order_id, amount in orders.items():
    print(
        f"{order_id}: order={fmt(amount)} valid_paid={fmt(valid_payment_by_order[order_id])} "
        f"refund={fmt(refund_total_by_order[order_id])} net={fmt(net_by_order[order_id])} "
        f"unpaid_before_refunds={fmt(unpaid_by_order[order_id])}"
    )
print("FINAL FIELDS")
print(f"refund_ids={settled_refund_ids}")
print(f"applied_refund={fmt(applied_refund)}")
print(f"net_received={fmt(net_received)}")
print(f"unpaid_before_refunds={fmt(unpaid_before_refunds)}")
