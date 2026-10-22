# GPT-6 Luna 上下文计量真实请求复测

> 历史实测记录：保留下述数据与当时的判断。后续精确计数实验已于 2026-10-21
> 撤回；当前实现不调用 OpenAI 计数接口，而使用本地估算、正常响应的 usage
> 校准及压缩/超窗恢复。本文不代表当前代码的重新验收结果。

测试日期：2026-10-21。使用用户指定的 `gpt-6-luna`，通过 OpenAI 官方
Responses API 发出 24 次真实生成请求。所有响应返回的模型均为 `gpt-6-luna`，
没有替换模型。普通模式为 `reasoning.effort=none`，思考模式为 `high`。

**结论：usage 校准与持久化回放有效，稳定追加的预测误差约 0.3%；工具调用和
频繁压缩仍有明显低估，15% 估算余量不能当作安全保证。**

## 接入与比较方法

该模型在思考模式下的工具调用使用 Responses；模型文档给出的窗口为
1,050,000、输出上限为 128,000 tokens。
[官方模型说明](https://developers.openai.com/api/docs/models/gpt-6-luna)

真实 `/models` 确认模型可访问，但不返回容量字段，因此本次适配器使用上述
文档值，并在报告中明确标为 `official_model_documentation_fallback`。
默认策略据此得到：输入上限 997,500、压缩触发点 945,000、目标 708,750、
优先保留近期历史 24,000。实际压缩测试另用较小预算，未触及 1M 边界。

OpenAI 与上一轮 DeepSeek **复用同一份 Rust 精度和 runtime 场景代码**，
没有为不同模型修改合成语料、提示或功能断言。精度场景分别使用英文、中文、
代码/JSON，每组约 30 KB：先发送 28 KB，再追加约 2 KB。两种模式各 6 次请求。

首轮响应及 usage 写入 JSONL，丢弃会话、存储实例和 context manager 后重新
打开，创建新管理器准备后续请求。为继续探针会话，首轮 `Stop` 仅在测试生命周期
中映射为 `InputRequired`；实际内容、usage、provider state 保持不变。独立
runtime 场景使用真正的模型驱动执行循环。本测试并非完整应用进程退出再启动。

Responses 使用 `store:false`，通过本地持久化的 `provider_state` 回放完整
有序 output items，包括加密思考状态；不依赖 `previous_response_id` 或服务端
保存会话。所有请求和响应均确认 `reasoning.context=all_turns`，避免把服务端
可能排除旧思考的 `current_turn` 与全历史回放混为一谈。
[思考状态与无状态回放说明](https://developers.openai.com/api/docs/guides/reasoning)

额外回放成本使用服务端 reasoning token 数作估算，不按密文长度计算，也不把
它称为精确的下一轮输入占用。缓存输入保留在输入总量中；reasoning 已包含在
输出总量内，不重复累加。

## 精度与 JSONL 恢复

以下 MAPE 为逐请求绝对百分比误差的平均。raw、prediction、decision 均在该次
请求前计算，再与该次服务端 `input_tokens` 比较；没有用本次响应反推预测。
decision 在 prediction 上仅给未测部分增加 15% 余量。

| 模式 / 阶段 | 请求数 | raw MAPE | prediction MAPE | decision 覆盖实际输入 |
| --- | ---: | ---: | ---: | ---: |
| none / 冷启动 | 3 | 4.86% | 4.86%（无基线） | 3/3 |
| none / JSONL 恢复后追加 | 3 | 4.85% | **0.31%** | 3/3 |
| high / 冷启动 | 3 | 4.86% | 4.86%（无基线） | 3/3 |
| high / JSONL 恢复后追加 | 3 | 4.83% | **0.30%** | 3/3 |

| 模式 | 语料 | 恢复后 raw | prediction | 实际输入 | prediction 相对误差 |
| --- | --- | ---: | ---: | ---: | ---: |
| none | 英文 | 7,587 | 7,522 | 7,516 | +0.08% |
| none | 中文 | 7,587 | 7,169 | 7,137 | +0.45% |
| none | 代码/JSON | 7,587 | 8,150 | 8,184 | −0.42% |
| high | 英文 | 7,596 | 7,531 | 7,527 | +0.05% |
| high | 中文 | 7,601 | 7,183 | 7,153 | +0.42% |
| high | 代码/JSON | 7,596 | 8,159 | 8,195 | −0.44% |

两种模式 12 次请求全部完成，6 次恢复均重建 `provider_calibrated` 基线，
并与相应首次实际输入用量一致。high 的三次首次请求分别产生 9、14、9 个
reasoning tokens，恢复后的请求各回放 1 个 reasoning item；这三次后续响应
自身的 reasoning tokens 均为 0。不能写成每次 high 请求都产生了新思考。

## 真实工具调用与连续压缩

high 模式完成 7 次主模型请求、4 次摘要请求、4 轮业务工具、1 次历史搜索和
1 次历史读取。25/25 项功能断言通过，28 个 canonical 事件保留完整：

- 4 次压缩检查点均前进并降低投影计量。
- 每次主请求完整保留用户约束，最终 9 个字段全部正确。
- 模型通过历史工具找回普通投影中被裁剪的 `CEDAR-7Q9`。
- 独立读取能恢复原始工具输出，存储事件与原始事件一致。
- 每次主请求均关联实际 runtime 计量快照，思考状态在工具调用之间成功回放。

| 主请求 | 来源 | raw | prediction | decision | 实际输入 |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | estimated | 394 | 394 | 454 | 315 |
| 2 | estimated | 7,095 | 7,095 | 8,160 | 8,433 |
| 3（压缩后） | estimated | 7,613 | 7,613 | 8,755 | 8,905 |
| 4（压缩后） | estimated | 7,638 | 7,638 | 8,784 | 8,913 |
| 5（压缩后） | estimated | 7,641 | 7,641 | 8,788 | 8,913 |
| 6 | provider_calibrated | 8,034 | 9,306 | 9,365 | 9,318 |
| 7（压缩后） | estimated | 7,592 | 7,592 | 8,731 | 9,355 |

主请求 raw MAPE **16.67%**，prediction MAPE **14.71%**。
仅第 1、6 次 decision 覆盖实际输入，其余 5 次即使加入 15% 余量仍低估。
第 7 次实际 9,355、decision 8,731，仍差 624 tokens。

第 2 次因首次工具输出裁剪后新增两个历史工具，工具 schema 变化，原基线失效。
第 3、4、5、7 次因摘要替换消息前缀而失效。回退避免把旧 usage 错用于新请求，
但也让底层粗估误差重新出现。唯一稳定延续的第 6 次预测误差为 −0.13%。

测试预算仍与 DeepSeek thinking runtime 相同：本地窗口 32,768，压缩触发点
9,000、目标 7,500、近期预算 6,500，工具结果投影限制 25,000 字符，主模型及
摘要各预留 8,192 输出。真实服务端窗口仍为 1M；功能通过不能证明实际超窗恢复。
4 个检查点的压缩后计量分别为 8,755、8,784、8,788、8,731，均未降到 7,500
的软目标。不能把“计量下降”写成“达到压缩目标”，也不能把这些本地投影数字
当作压缩前后服务端实测。包含摘要在内，本轮最大实际输入仅 9,656 tokens。

## 与 DeepSeek 上轮结果对照

| 指标 | 本次 GPT-6 Luna | DeepSeek 上轮 |
| --- | ---: | ---: |
| 普通模式，恢复后追加 prediction MAPE | 0.31% | 1.19% |
| 思考模式，恢复后追加 prediction MAPE | 0.30% | 1.18% |
| 思考模式，工具与压缩 prediction MAPE | 14.71% | 17.76% |
| 精度场景 decision 覆盖 | 12/12 | 8/12 |
| runtime 主请求 decision 覆盖 | 2/7 | 1/7 |

这些是当前 harness 估算器对不同模型、协议和内容的适配结果，不是模型能力排名，
也不是旧版与新版算法的 A/B 提升。OpenAI 用 Responses `all_turns`，DeepSeek
用 Chat Completions 且无工具时不计入历史思考，协议差异不能忽略。
原始语料虽然相同，生成的工具调用、摘要、推理长度仍由各模型决定。

两轮实测共同说明：已测历史占大部分时，usage 基线显著降低整体误差；新工具
集合、较大的工具内容与压缩后的上下文，需要更准确的 provider 计数方案。
不能据这 3 类重复结构的语料推断通用安全比例，也不能把约 0.3% 推广到全部任务。

## 用量、证据与验证

共 24 次生成请求：连通性 1 次、精度 12 次、runtime 11 次。
输入 **179,078 tokens**，输出 **3,512 tokens**；输入中缓存命中 **51,478**，
输出中 reasoning **1,371**。无网络失败，未自动重试，未挑选重跑结果。
所有发送内容均为合成数据。密钥未写入文件，持有密钥的临时进程已经退出。
报告不保存加密思考内容或原始思考文本。

- [连通性原始记录](results/openai-context-none-probe-token-meter-20261021.json)
- [普通模式精度记录](results/openai-context-none-precision-token-meter-20261021.json)
- [思考模式精度记录](results/openai-context-high-precision-token-meter-20261021.json)
- [思考模式 runtime 记录](results/openai-context-high-runtime-token-meter-20261021.json)
- [DeepSeek 上轮报告](deepseek-token-measurement-report.md)
- [运行说明](../crates/agent-runtime/examples/openai_context/README.md)

本轮新增可复用 OpenAI Responses 测试适配器和入口，并将 precision 函数泛型化
供两个 example 共用。没有修改生产计数算法或默认余量。两个 example 的离线
测试分别 15/15 通过（有共享测试，不应累加成 30 个独立用例），两者 Clippy、
工作区格式检查、`git diff --check` 及传输错误脱敏检查均通过。

本轮不覆盖近 1M 输入、真实超窗恢复、网络流式响应、重试、服务端原生压缩、
其他 OpenAI 型号或长期任务可靠性。
