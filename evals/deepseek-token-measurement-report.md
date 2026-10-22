# DeepSeek token 计量真实请求复测

> 历史实测记录：保留下述数据与当时的判断。文中建议引入 tokenizer/count API
> 的方向已于 2026-10-21 撤回；当前方案继续使用本地估算、正常响应的 usage
> 校准及压缩/超窗恢复。本文不代表当前代码的重新验收结果。

日期：2026-10-21。使用官方 API 的 `deepseek-flash`；`/models` 返回
`DeepSeek-V4.1-Flash`、1,048,576 tokens 窗口，没有独立返回不可变的
`1017` 修订号。测试覆盖普通模式和 `thinking.type=enabled`、
`reasoning_effort=high` 的思考模式。

**结论：服务端 usage 校准及 JSONL 恢复有效，但当前粗估仍不够可靠。**
稳定历史上追加少量内容时，校准预测的平均绝对百分比误差约为 1.2%；
工具集合变化、较大的新增工具结果和压缩后的新上下文仍存在明显低估，
当前 15% 的估算余量未能覆盖全部样本。功能通过不代表预算精度验收通过。

## 方法和口径

- 三组固定合成语料：英文、中文、代码/JSON，每组约 30 KB。首次发送
  28 KB，随后追加约 2 KB；两种模式各 6 次真实请求。单次实际输入约
  5,296–9,158 tokens。这是小规模、重复结构的受控样本，不是通用语料基准。
- 使用实际 `CompactingContextManager.prepare/measure_request`。首轮响应和
  usage 写入 `JsonlEventStore`，关闭存储并丢弃会话与管理器，再从 JSONL
  恢复，创建新管理器，准备第二次请求。6 组恢复均成功使用真实 usage 基线。
- 为继续两轮探针会话，仅将首轮完成原因从 `Stop` 映射成 `InputRequired`；
  内容、usage、provider state 保持原样。实际执行循环另由 runtime 场景验证。
- raw 是本地计数器的未校准估算；prediction 是已测前缀加未测增量；
  decision 是 prediction 再加未测部分的 15% 余量。它们都在发送该次请求前
  计算，再与该次服务端 `prompt_tokens` 比较，不使用该次响应反推预测。
- 相对误差为 `(预测 − 实际) / 实际`；MAPE 是逐请求相对误差绝对值的平均。
  缓存 token 是输入的子集，仍占上下文。reasoning token 是输出的子集，不重复相加。
- 下列 raw 与 calibrated 比较针对**同一次请求**，不是旧版与新版完整算法的 A/B 测试。

## 稳定前缀与恢复结果

| 模式 / 阶段 | 请求数 | raw MAPE | calibrated prediction MAPE | decision 覆盖实际输入 |
| --- | ---: | ---: | ---: | ---: |
| 普通 / 冷启动 | 3 | 17.18% | 17.18%（无基线） | 2/3 |
| 普通 / JSONL 恢复后追加 | 3 | 17.22% | 1.19% | 2/3 |
| 思考 / 冷启动 | 3 | 16.93% | 16.93%（无基线） | 2/3 |
| 思考 / JSONL 恢复后追加 | 3 | 16.99% | 1.18% | 2/3 |

恢复后各组的实际值与预测：

| 模式 | 语料 | raw | prediction | 实际输入 | prediction 相对误差 |
| --- | --- | ---: | ---: | ---: | ---: |
| 普通 | 英文 | 7,587 | 7,519 | 7,507 | +0.16% |
| 普通 | 中文 | 7,587 | 5,812 | 5,676 | +2.40% |
| 普通 | 代码/JSON | 7,587 | 9,041 | 9,133 | −1.01% |
| 思考 | 英文 | 7,587 | 7,544 | 7,532 | +0.16% |
| 思考 | 中文 | 7,587 | 5,837 | 5,701 | +2.39% |
| 思考 | 代码/JSON | 7,587 | 9,066 | 9,158 | −1.00% |

代码/JSON 的余量不足是本轮的重要发现：普通冷启动 decision 为 8,132、
实际 8,525，少 393；思考冷启动实际 8,550，少 418。恢复后仍分别少 14。
12 次精度请求中只有 8 次 decision 覆盖实际输入。测试没有调整余量或挑选重跑
结果来掩盖不足，`precision.passed` 只代表请求完成和持久化校准契约通过。
恢复场景的低总误差主要来自大部分历史已被实测覆盖。代码/JSON 的新增部分
仍估为 516、实际增加 608，增量自身低估 15.13%；中文新增部分则高估。

## 思考模式下的执行与压缩

实际 `AgentRuntime` 完成 7 次主模型请求、4 次真实摘要请求、4 轮业务工具、
1 次历史搜索和 1 次历史读取。25/25 项功能断言通过：

- 4 次压缩均保存检查点，边界前进，投影计量下降。
- 每次主请求均完整保留用户约束，最终 9 个字段全部正确。
- 模型通过历史工具找回普通投影中被裁剪的 `CEDAR-7Q9`。
- 原始工具输出能完整读取，canonical 事件与存储一致。
- 每次主请求都匹配实际运行时计量快照；思考模式由非零 provider reasoning usage 验证。

| 主请求 | 计量来源 | raw | prediction | decision | 实际输入 |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | estimated | 394 | 394 | 454 | 569 |
| 2 | estimated | 7,077 | 7,077 | 8,139 | 8,772 |
| 3（压缩后） | estimated | 7,844 | 7,844 | 9,021 | 9,505 |
| 4（压缩后） | estimated | 7,919 | 7,919 | 9,107 | 9,583 |
| 5（压缩后） | estimated | 7,971 | 7,971 | 9,167 | 9,640 |
| 6 | provider_calibrated | 8,408 | 10,077 | 10,143 | 10,143 |
| 7（压缩后） | estimated | 8,279 | 8,279 | 9,521 | 10,535 |

这 7 次主请求 raw MAPE 为 20.11%，prediction MAPE 为 17.76%。仅第 6 次
使用匹配基线，其预测误差为 −0.65%；该次 decision 恰好等于实际输入，不能
据此推断余量总能覆盖。其余 6 次 decision 均低于实际输入。
压缩改变了消息前缀，旧基线失效后重新使用估算是正确的隔离行为，但暴露了
底层计数器的精度问题。稳定追加的约 1.2% 误差不能推广到压缩和工具密集场景。
第 2 次主请求尚未压缩，但首次工具输出被裁剪后新增了两个历史工具，工具
schema 从 1 个变为 3 个，固定指纹因此不再匹配，旧基线同样不能直接沿用。

此场景使用 32,768 的测试预算、9,000 触发点、7,500 目标、6,500 近期预算，
工具输出投影限制 25,000 字符；主模型与摘要输出各预留 8,192。它们是为了用
有限调用触发压缩的测试设置。真实服务端窗口仍是 1M，未触及物理窗口边界。

## 范围与后续方向

本轮未验证真实超窗恢复、近 1M 输入、流式网络、限流/断网重试、其他厂商协议、
服务端原生压缩或长期任务可靠性。旧 large/near-window 报告不能作为本轮新策略
在大输入上的验证结果；当前估算余量也会改变旧 near-window 样本的准备结果。

后续应优先改进无基线和压缩后的计数：接入匹配模型/协议的 tokenizer 或服务端
count API（若可用），并基于更广的真实语料评估新增内容和工具 schema 的误差。
可评估按模型、内容类型学习保守增量系数，但需要独立样本验证；不能把 15%
常量视为不超窗保证。服务端超窗恢复仍应作为最后一道防线。

## 证据、复现与验证

共 24 次真实生成请求（1 次连通性、12 次精度、11 次 runtime）：
输入 183,278 tokens，输出 6,037 tokens；输入中缓存命中 64,384，输出中
reasoning 为 2,039。无网络错误，未自动重试。所有输入均为合成数据。
密钥未写入文件；持有密钥的临时进程已退出。报告不包含原始思考文本。

- [普通模式原始报告](results/deepseek-context-precision-token-meter-20261021.json)
- [思考模式原始报告](results/deepseek-context-thinking-precision-token-meter-20261021.json)
- [思考模式 runtime 原始报告](results/deepseek-context-thinking-runtime-token-meter-20261021.json)
- [连通性报告](results/deepseek-context-probe-token-meter-20261021.json)

在环境中提供 `DEEPSEEK_API_KEY` 后运行：

```sh
cargo run -p agent-runtime --example deepseek_context --locked -- precision non-thinking token-meter
cargo run -p agent-runtime --example deepseek_context --locked -- precision thinking token-meter
cargo run -p agent-runtime --example deepseek_context --locked -- runtime thinking token-meter
```

新增可复用 precision 场景及 runtime 计量插桩。本轮 example 离线测试 15/15、
example Clippy、工作区格式检查和 `git diff --check` 均通过；未重复运行与本轮
评估代码无关的桌面端测试。本轮没有调整生产计数算法或 15% 默认余量。
