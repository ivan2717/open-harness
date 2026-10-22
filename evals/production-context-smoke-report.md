# 生产模型接入与上下文恢复验证

本轮通过桌面使用的 Rust API 服务和新 `agent-providers` HTTP 客户端运行，
不再借用 evaluation-only Python HTTP adapter。
原始结果：[production-context-smoke-20261021.json](results/production-context-smoke-20261021.json)。

| 验证项 | DeepSeek `deepseek-flash` | OpenAI `gpt-6-luna` |
| --- | --- | --- |
| 协议 / 思考配置 | Chat Completions / DeepSeek high | Responses / high |
| 成功主会话响应 | 4 | 4 |
| 手动摘要检查点 | 1 | 1 |
| 压缩后保留项目代号与阈值修正 17 → 23 | 通过 | 通过 |
| 重启、重新配置同一模型后继续对话 | 通过 | 通过 |
| 持久化 provider state / usage 锚点 | 4 / 4 | 4 / 4 |
| 思考状态 | 4 个可见 reasoning 块 | 2 个 encrypted reasoning item |
| 重启后的输入预测误差 | +1.22% | 0.00% |

两组均使用提供方别名，未独立验证不可变模型修订。测试声明 1M 容量，
实际输入约千级 tokens；这不是 1M 边界或长时稳定性测试。
密钥通过隐藏终端输入进入进程内存，未写入源码、参数、运行快照或报告。

计数沿用本地 UTF-8 估算和正常响应 usage 校准，没有 tokenizer 或额外计数请求。
每组依次检查冷启动、连续请求、首次压缩后的请求和重启后的请求。
四次输入预测的有符号误差分别为：

- DeepSeek：+24.69%、+3.14%、−8.41%、+1.22%。
- OpenAI：−2.55%、+0.10%、+20.35%、0.00%。

首次压缩启用历史回读工具，固定工具定义随之变化，因此该次回退本地估算；
下一次正常响应建立新锚点，重启后能恢复校准。相同固定头部下摘要替换的校准
连续性另由确定性单元和持久化测试覆盖。本轮样本很小，不能据此宣称整体误差上界。

另有 Electron 本地端到端测试覆盖三个协议，共 16 个 HTTP fixture 请求，
包含 4 个摘要请求（其中 1 个主动取消），验证配置、取消、压缩和重启后的连续对话。
Messages 使用本地协议测试，未调用真实 Anthropic API。
