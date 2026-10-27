import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

// Only test inputs live here. Expected answers are kept in the independent scorer.
export const INPUTS = Object.freeze({
  "orders.csv": `order_id,amount
O101,120
O102,80
O103,200
O104,60
O105,150
O106,90
`,
  "payments.csv": `payment_id,order_id,amount,status
P201,O101,120,paid
P202,O102,80,paid
P203,O103,200,paid
P202,O102,80,paid
P204,O105,75,paid
P205,O999,50,paid
P206,O104,60,failed
P207,O106,90,cancelled
`,
  "refunds.csv": `refund_id,payment_id,order_id,amount,status
R301,P203,O103,30,settled
R302,P201,O101,20,settled
R303,P204,O105,10,pending
R304,P999,O102,5,settled
`,
  "rules.md": `# 对账规则

金额单位为元；材料只涉及这一个结算期间，不需要汇率、税费或外部资料。

1. 订单金额按 orders.csv 的唯一订单统计。
2. 付款先按 payment_id 去重；同 ID 的重复行内容一致，只计一次，并报告重复 ID。
3. 仅 status=paid 且能匹配 orders.csv 中订单的付款计入有效付款。failed、cancelled 均不计入，并列出其付款 ID。
4. status=paid 但订单不存在的付款列为孤立付款，不计入订单回款。
5. 退款只有 status=settled、匹配已计入的付款，且 order_id 与该付款一致时才冲减净回款。pending 不冲减；已结算但付款无法匹配的退款列为孤立退款。
6. 每个订单的净回款 = 有效付款 - 已生效退款。全局指标为逐订单求和。
7. unpaid_before_refunds 是 max(订单金额 - 有效付款, 0)，退款不增加这个指标。未付款订单是有效付款为 0 的订单；部分付款订单是有效付款大于 0 但小于订单金额的订单。
8. 来源文件不能修改。仅可在 outputs/ 中交付结果，在 .openharness-tmp/ 中写临时脚本。
9. 不要联网，不要安装依赖，不要读其他项目。依据只能来自本目录材料；没有依据的信息应明确说明。
`,
});

export async function createFixture(workspace) {
  await mkdir(workspace, { recursive: true });
  for (const [name, content] of Object.entries(INPUTS)) {
    await writeFile(join(workspace, name), content, { flag: "wx" });
  }
  return Object.keys(INPUTS);
}

const outputContract = `请读取本目录的 orders.csv、payments.csv、refunds.csv、rules.md，按照规则核对订单、付款和退款，交付：
- outputs/reconciliation.json：机器可核对的结构化结果。
- outputs/report.md：中文对账报告，概括计算口径、结论和异常，注明来源文件名和记录 ID。

JSON 必须使用以下结构（所有金额均为 JSON 数字，不得带币种符号；ID 列表不得重复）：
{
  "summary": {
    "order_total": 金额,
    "matched_paid": 金额,
    "applied_refund": 金额,
    "net_received": 金额,
    "unpaid_before_refunds": 金额
  },
  "orders": [{
    "order_id": "原订单 ID",
    "paid": 金额,
    "refund": 金额,
    "net": 金额,
    "unpaid_before_refunds": 金额,
    "payment_ids": ["实际计入的付款 ID"],
    "refund_ids": ["实际生效的退款 ID"]
  }],
  "anomalies": {
    "duplicate_payment_ids": ["重复付款 ID"],
    "orphan_payment_ids": ["孤立付款 ID"],
    "orphan_refund_ids": ["孤立退款 ID"],
    "unpaid_order_ids": ["未付款订单 ID"],
    "partial_order_ids": ["部分付款订单 ID"],
    "excluded_payment_ids": ["失败或取消的付款 ID"],
    "pending_refund_ids": ["未结算退款 ID"]
  }
}

orders 必须包含所有原始订单，payment_ids 与 refund_ids 必须与本行计算一致，无记录时使用空数组。报告注明原文件和关键记录 ID；不能只声称已检查。不要修改原始材料；无需联网或安装依赖。完成后检查两个输出文件确实存在并能读取。`;

export function promptFor(mode) {
  const instructions = {
    single:
      "本次使用单 agent 完成全部工作，不启动任何子 agent。请自行复核数字和依据。",
    auto: "请自行判断是否需要子 agent 协作。若使用协作，避免重复工作；主 agent 负责复核数字和依据并交付最终文件。",
    delegated:
      "本次必须使用两个子 agent：一个负责订单回款、退款和金额核对，一个负责重复、孤立、失败、取消及未付款记录检查。给它们明确规则与交付要求。主 agent 收齐结果后独立复核关键数字与依据、处理分歧，并写最终文件。子 agent 不要同时写最终交付文件。",
  };
  if (!Object.hasOwn(instructions, mode))
    throw new Error(`Unknown evaluation mode: ${mode}`);
  return `${instructions[mode]}\n\n${outputContract}`;
}
