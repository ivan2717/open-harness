# DeepSeek 思考模式上下文实测

日期：2026-10-21。模型 API 名称 `deepseek-flash`，官方当前映射为
DeepSeek-V4.1-Flash-1017。所有主模型与摘要模型请求均显式设置
`thinking.type=enabled`、`reasoning_effort=high`；返回的真实 reasoning token
统计验证了思考模式确实开启。输入为合成数据，没有上传仓库源码。

**结论：加入测试适配器的思考回传后，当前进程内的大输入、连续压缩和历史回查
可以正常工作。但核心 harness 尚不能据此认定为完整支持思考模式：思考状态没有
持久化到事件和检查点，而且真实用量校准存在无工具场景下的高估。**

## 真实 API 结果

| 场景 | 实测结果 |
| --- | --- |
| 超过旧 128K 窗口 | 输入 154,136 tokens；3 个标记全部正确；无摘要调用 |
| 大输入 | 输入 875,136 tokens；3 个标记全部正确；无摘要调用 |
| 工具对话及连续压缩 | 7 次主模型请求、4 次真实思考摘要；24 项上下文断言全部通过 |
| 思考内容回传 | 第一轮生成 303 个 reasoning tokens；下一轮实际回传 1,338 字符的原始思考内容 |
| 压缩后的回传 | 只回传仍在活动上下文中的 assistant 工具轮次，没有重新插入被压缩掉的轮次 |
| 被裁剪内容回查 | `search_run_history` 一次定位事件 4，再读取该事件；找回 `CEDAR-7Q9` |
| 摘要输出截断 | 32-token 上限下服务端返回 `length`；软阈值安全回退，硬超限拒绝继续；不写坏检查点 |
| 无工具用量校准 | 当前公式预测下一轮输入 193 tokens；服务端实际 100，发现高估 |

工具场景仍使用 32,768 的测试窗口、9,000 的压缩触发点、7,500 的压缩目标、
6,500 的近期历史预算和 25,000 字符的工具投影上限。思考模式把主模型与摘要
模型输出预留分别提高到 8,192，因为该预算同时包含思考和可见输出；摘要模型
总窗口仍为 65,536。默认模型窗口策略没有修改。

连续压缩后的检查点预算计数为：

| 压缩到 sequence | 压缩前 | 压缩后 |
| --- | ---: | ---: |
| 4 | 15,384 | 7,769 |
| 8 | 15,752 | 7,848 |
| 12 | 15,841 | 7,928 |
| 16 | 9,967 | 2,013 |

这些是运行时预算计数，不是服务端输入 token 的逐项实测值。

另对保存的实际编码请求指标执行了 7 项断言：思考模式为 high、确实回传非空
思考文本、历史读取之前没有归档值、读取之后出现归档值、缓存思考没有提前泄露
该值，以及场景调用次序和最后一次工具读取的一致性，全部通过。没有保存思考
文本，只保存 token 数、字符长度、回传条数和合成标记是否出现。

## 发现的问题

### 1. 文本事件链没有保存 provider 思考状态

当前 `ModelResponse` 和原始事件只保留可见文本、工具调用与结果；文本摘要链
无法从事件存储恢复 DeepSeek 的 `reasoning_content`。本轮为了把真实协议跑通，
在 example 适配器内新增了最多 256 条的进程内缓存：

- 按 assistant 文本以及完整、有序的工具 ID、名称、参数精确匹配原始响应。
- 恢复完整原始 assistant 消息，包括 `reasoning_content`，不伪造空思考。
- 压缩后仅保留活动轮次；找不到匹配状态时明确报错。
- 对实际回传的思考文本补入完整请求及前缀的 token 估算。

这验证了**同一个进程内的适配可行性**，没有增加核心事件持久化能力。
进程重启、缓存丢失、多个并发 run，以及无工具 assistant 历史后来转为工具对话，
都不能据此宣称已经支持。现有 `provider_context`/native manager 提供另一种状态
通道，但本次没有验证它与 DeepSeek 思考及压缩的完整适配。

在原样省略思考回传的对照中，模型产生过非空思考，但 API 没有返回预期的 400，
任务也完成了。这与文档描述的强制检查不一致，因此不能用“没有报错”证明状态
没有丢失。保留实际观察，不把文档预期当成测试结果。

首轮原始对照报告中的 `negative_control.passed=false` 表示“必定报 400”的
测试假设未成立；其内部 runtime 检查通过。测试入口随后改为记录实际拒绝情况，
不再假定服务端一定执行该校验。

### 2. 把未保留的思考输出计入下一轮输入

现有 `context/usage.rs` 的基线采用：

```text
上一轮 input_tokens + 上一轮全部 output_tokens + 新消息估算增量
```

DeepSeek 的无工具对话不会把历史思考接入下一轮上下文。本次真实请求为：

| 项目 | tokens |
| --- | ---: |
| 第一轮输入 | 77 |
| 第一轮总输出 | 93 |
| 其中思考输出 | 82 |
| 新消息估算增量 | 23 |
| 当前公式预测下一轮输入 | 193 |
| 服务端实际下一轮输入 | 100 |
| 预测高于实际 | 93 |

该测量使用与现有 tracker 一致的公式，第一轮可见回答原样进入第二轮请求；
没有把假用量写回 runtime。差额不要求精确等于 82，因为可见文本和消息封装仍
有估算差异，但该样本明确显示“全部生成输出都成为下一轮输入”的假设不成立。

结果可能是提前压缩，甚至在很紧的预算下误判超限。只为 `TokenCounter` 加上
思考文本估算不能解决这个问题，因为确认后的基线已经使用了全部输出用量。
后续应区分完整生成用量与实际保留的上下文 token，或者仅校准已确认输入前缀，
对真正追加的响应内容另外计数。

**本轮没有修改该核心计数语义，也没有把实验缓存当成生产支持合入运行时。**

## 证据和复现

- `results/deepseek-context-thinking-drop-runtime.json`：省略回传的原始对照，11 次请求。
- `results/deepseek-context-thinking-all.json`：带回传的全部场景，17 次请求。
- `results/deepseek-thinking-wire-verification.json`：对保存的实际编码请求指标的 7 项断言。

本轮共 28 次真实 Chat Completions 请求，输入 1,219,946 tokens、输出
14,563 tokens，其中 reasoning tokens 为 7,414；缓存命中输入 97,536。
缓存数是输入的子集，思考数是输出的子集，没有重复加总。两次摘要 `length`
是有意制造的失败；没有遇到网络或限流错误。

环境中设置 `DEEPSEEK_API_KEY` 后：

```sh
cargo run -p agent-runtime --example deepseek_context --locked -- all thinking
cargo run -p agent-runtime --example deepseek_context --locked -- runtime thinking-drop
cargo run -p agent-runtime --example deepseek_context --locked -- accounting thinking
```

当前检查包括 165 项离线测试，以及 Clippy、格式和 diff 检查。
本轮新增代码主要在 example 的 `model.rs`、`replay.rs`、`accounting.rs` 和场景入口。
普通测试不会联网。密钥没有写入代码或报告，持有密钥的测试进程已退出。

参考：
[思考模式及回传规则](https://api-docs.deepseek.com/guides/thinking_mode/)、
[输出预算及 usage 字段](https://api-docs.deepseek.com/api/create-chat-completion/)。
