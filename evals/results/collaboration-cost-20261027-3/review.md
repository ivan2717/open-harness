# 成本优化第二版：第 3 轮

2026-10-27。原评分 **16/16**、追加审计 **26/26**，独立脚本与最终产物人工复核通过。已观测 **377,274 tokens**，32 次响应，耗时 **266,815 ms**；比基线 377,225 多 49 tokens，不能称为节省，且超过 220k。

模型和条件仍为 gpt-6-luna / Responses / standard / medium，24,576 窗口、4,096 最大输出，450,000 已观测 token / 480 秒护栏，原固定业务提示、四份来源、更正、四轮归档未变。第二版冻结 API SHA-256：`dc0c24b6d8ece966791026b4631891bfdffb3e8210c660b0f408cd22f028d158`，加入了宿主确认的 execution_evidence；与第 1/2 轮版本分开记录。

## 完成性和质量

主 `run-18e251bd7339d348-46b6-1` 在交付前真实压缩覆盖更正 event-4，总计两次压缩。唯一子 `run-18e251ce4600f760-46b6-2` 用一次 parent read 获取 event-0 与 event-4 完整原文预览，未重复精读。父下发任务没有金额答案。

子创建 `.openharness-tmp/reconcile_independent.py`，实际从 CSV 读取金额，用 Decimal 按付款 ID 去重、paid 状态及订单匹配过滤，再匹配退款并单独对 R303 使用 settled 更正。金额输出由来源计算，末尾常量仅用于断言计算结果，未替代计算。子命令 `command-1793085574673759000-1` 实际执行，completed / exit 0 / network denied / read_only；完整输出含逐记录分类、逐订单计算与断言成功。

独立只读重算确认退款 R301/R302/R303、退款 60、净回款 415、退款前欠款 225。最终 JSON 数字与引文正确，P202/P205/P206/P207/R304 的去重和排除解释准确，来源逐字未变。主读取四份来源、脚本和子报告，写文件并两次 read_file 读回；自己的 `command-1793085626416689000-1` 实际执行 Node JSON.parse、关键数字及更正引用断言，exit 0。没有运行后补写产物或补跑模型脚本。

## execution_evidence 送达，但重复查询仍发生

自动完成通知完整包含子报告，以及宿主工具结果产生的：command ID、completed、exit 0、cleanup_completed、host_cleanup_completed 和精确输出消息 `tool-call_Em3uHmga7zYj2y0I5oILoBVy`。主随后调用 result(include_reports:false)，也得到同一执行证据，未重复复制报告正文。

但父仍进行了 **4 次按 offset 读取子工具历史**，offset 依次为 0、8、0、2；没有按给出的命令 output_message_id 精确读取命令输出。之后另按 ID 读取完整子报告 event-27，以及子读父历史的工具消息 `tool-call_sgQxQNura66Y1fe2Z50kKERI`；后者不是命令输出。二次压缩后又重新读取四份来源与最终 JSON、搜索已成功的解析断言结果并查询成员列表。第二次摘要已明确写“任务已完成”“无需继续计算或修改”，重复核验仍发生。

因此，本轮能证明新的宿主执行证据已可靠送达，但不能证明模型已经改用该最短取证路径；不得把接口能力直接当作成本改善结果。

| 执行者 | tokens | 响应 |
| --- | ---: | ---: |
| 主任务 | 327,410 | 27 |
| 子任务 | 49,864 | 5 |
| 合计 | 377,274 | 32 |

主子均正常结束，无遗留命令或审批。launcher 退出 0，临时 API 状态及本轮专用 Electron 目录已删除，公开证据未检出 key 样式内容。见 `cleanup-verification.json`。这是固定用例的一轮观察，已观测 tokens 非完整账单，不能宣称统计显著、稳定节省或一般质量保证。
