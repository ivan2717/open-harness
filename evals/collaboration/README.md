# 主子 agent 验收

这一套验收把系统机制、真实模型产物、协作成本分开记录。模型说“已完成”不算通过依据。

## 固定业务题

`fixture.mjs` 写入 6 个订单、8 行付款、4 行退款及规则。三种模式只改变协作要求：

- `single`：不启动子任务。
- `auto`：自行判断是否分工。
- `delegated`：两个子任务分别核对金额和异常，主任务复核并交付。

每次使用新会话和新目录。标准答案保存在 `score.mjs`，不写入受测工作区；不应把仓库本身选为受测项目。固定小样本适合检测功能和开销，不能代表复杂任务的总体质量。

`score.mjs` 检查数字、订单集合、记录依据、异常集合、源文件最终哈希以及报告基本存在性。报告语义、主任务是否核验、工具操作是否合理，必须另读公开记录复核。`result.passed` 仅表示自动验收通过，不能解释成所有行为和报告都没有问题。

```sh
node --test evals/collaboration/score.test.mjs evals/collaboration/continuity-evidence.test.mjs
cargo test -p api --test collaboration --test collaboration_undo
cargo test -p api --lib extensions::
```

## 真实模型执行

`live-electron.cjs` 使用 Electron 的 `safeStorage` 读取已有模型保险库，仅在内存中把配置传给隔离的测试 API；不会修改原保险库或输出密钥。它先输出可选模型的非敏感元数据，再从标准输入接收一次 JSON 配置。操作系统仍可能要求确认读取凭据。

执行前构建当前 API，并用桌面端 `prepareWorkspaceRuntime` 生成共享运行库清单。使用桌面端 Electron 运行：

```sh
cargo build -p api --bin api
# 以下变量分别指向已有加密保险库、专用 Electron 测试目录。
OPENHARNESS_EVAL_VAULT=/absolute/path/models.encrypted \
OPENHARNESS_EVAL_USER_DATA=/absolute/path/test-electron \
  /absolute/path/electron evals/collaboration/live-electron.cjs
```

看到模型列表后输入（所有路径必须替换为实际路径）：

```json
{"action":"run","model_id":"从列表选择","backend":"/absolute/path/api","output":"/absolute/path/new-results-directory","runtime_manifest":"/absolute/path/runtime.json","seconds_per_case":240}
```

这是会消耗真实模型用量的测试。默认顺序为 `single,auto,delegated / delegated,single,auto / auto,delegated,single`，每种 3 次；可以提供 `order` 数组缩小范围。输出目录必须尚不存在，以免覆盖证据或误重复运行。

每例最多 240 秒、每 agent 每轮 24 个模型步骤、最多 2 个并发子任务、每例主子任务合计已观测用量 180,000 tokens。这些是验收护栏，不是产品默认限制，也不是精确费用上限；在途请求及未记录用量可能超出观察值。

命令审批仅针对测试目录内、OS 沙箱可用且网络禁止的命令，逐条一次性批准并保存记录；不安装依赖，不连接个人 MCP 或浏览器。这个自动批准过程不测量真实用户审批时间。外部操作、拒绝及停止行为在独立机制套件中测试。

保存的文件包括配置元数据、逐例评分、命令审批、公开消息／事件及实际产物。报告不保存 API key 或私有推理；临时 API 状态含正常持久化数据，位置记录于元数据，不能当作脱敏公开报告上传。

## 压缩后继续协作

同一个入口输入 `action: "continuity"`，执行 `continuity.mjs`：先给出任务，再更正一条退款状态；插入无关资料并真正压缩，随后要求唯一子任务回查父会话原文、写脚本并执行，主任务复核交付。最终指令不重复修正值或标准答案。

```json
{"action":"continuity","model_id":"从列表选择","backend":"/absolute/path/api","output":"/absolute/path/new-continuity-results","runtime_manifest":"/absolute/path/runtime.json","seconds_per_case":480,"observed_token_limit":220000}
```

该测试仅在内存中把模型配置的窗口缩为 24,576、输出上限缩为 4,096，不修改已有模型配置。它测试压缩后的连续性，不是 1M 容量测试。默认累计已观测用量上限 220,000，允许显式设定到 450,000；上限停止必须记为未完成，不能通过改分或补写产物算成功。2026-10-27 两次尝试及失败原因保存在 `continuity-review-20261027.md`。

同日协作修复后的真实复验保存在 `../results/collaboration-continuity-20261027-collaboration-fix/`、`../results/collaboration-continuity-20261027-bounded/` 和 `../results/collaboration-continuity-20261027-functional-450k/`。前两轮 220k 预算内未完成；最后一轮沿用旧 450k 基线，上下文压缩、父历史回查、子脚本实际执行、父任务复算和 JSON 交付/读回全部通过，实际观测 377,225 tokens。每轮 `review.md` 记录独立证据复核与限制；这说明该固定用例功能通过，不代表 220k 效率目标或 1M 容量测试通过。

当前连续性证据评分标记为 `evidence_version: 2`，业务提示词和预期数字不变。历史读取必须返回真实父任务的更正消息 ID 和完整原文，因此 `parent` 别名与真实 ID 等价；最终引用必须指向实际更正编号或事件。脚本执行必须同时有子任务自己的宿主命令记录、相同 `command_id` 的成功工具结果，以及 `.openharness-tmp/` 内实际存在的脚本。任意命令返回 0 或仅在文本中提及脚本不会通过。自动判断识别直接的 Python、Node、Shell 或 PowerShell 脚本调用；复杂命令和脚本内部的业务逻辑仍需人工复核。旧报告保留原评分，不追溯改成新版分数。

本轮协作优化的最终真实复验保存在 `evals/results/collaboration-continuity-20261027-functional-450k/`：16/16 通过，独立检查了子任务脚本退出码、父任务复算、JSON 读回、来源引用和源文件哈希。耗时 182 秒，已观测 377,225 tokens。这是 450,000 测试预算内的功能完成性证据；同轮此前两次 220,000 预算测试均未完成，保存在 `collaboration-continuity-20261027-collaboration-fix/` 和 `collaboration-continuity-20261027-bounded/`。不能把预算提高后的通过解释为协作效率达标，也不能据单个样本宣称稳定成功率。

连续性测试结束会先导出公开记录和实际工作区，再删除自己创建的临时 API 状态。基础九例测试的临时状态由验收结束后的清理操作删除，清理回执保存在对应结果目录的 `cleanup.json` 中。

## 阅读结果

- `results.json`：逐次自动验收、时间、主子用量合计及实际子任务数量。
- `<case>/transcripts.json`：公开过程记录；可检查是否真的分工、读取原文、收齐结果和回读产物。
- `<case>/workspace/outputs/`：模型原始交付，不能事后修正再计为通过。
- 连续性结果的 `workspace-reviews.json`：按主子任务分别保存的命令记录，用于核对实际执行者和命令结果。
- `mechanisms-*.md`：模拟模型／MCP的确定性机制结果。
- `manual-review-*.md`：独立产物和协作证据复核，补充自动评分的语义盲区。

用量是主子 agent 已记录成功响应的输入／输出合计，不是精确账单，也不保证包含重试、压缩、Wiki 等所有调用。时间包含模型、工具和宿主轮询，不能解释为纯推理速度。三次重复仅提供小样本观察，不能宣称统计显著。
