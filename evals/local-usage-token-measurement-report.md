# 简化计数方案的真实 API 复测

日期：2026-10-21。测试的是删除 tokenizer、OpenAI 额外计数请求及整单计数缓存后的当前工作区代码。
本轮未修改计数算法、默认 15% 估算余量或测试语料，也未为了改善结果重跑或筛选样本。

**结果：功能检查通过，但工具调用与压缩场景仍明显低估。usage 校准只在原请求前缀及工具配置仍匹配时有效；15% 余量未覆盖全部请求。**

## 测试范围与口径

- DeepSeek：官方 API 的 `deepseek-flash`，目录名称为 `DeepSeek-V4.1-Flash`。请求按此前指定的 Flash 模型测试；服务端未独立确认不可变的 `1017` 修订号。
- OpenAI：官方 Responses API 的 `gpt-6-luna`，全部实际响应的模型字段均为 `gpt-6-luna`。
- 两模型各跑普通/思考模式精度测试，以及一次思考模式的工具与连续压缩测试。思考模式均通过非零 reasoning usage 验证。
- 精度场景为英文、中文、代码/JSON三组固定合成语料，各约30 KB；首轮约28 KB、追加约2 KB。首轮写入JSONL，重建存储和上下文管理器后继续。
- 预测在请求发出前记录，再与该次正常响应的实际输入 usage 比较。未使用本次响应修正本次预测；缓存输入包含在 input 总量中，reasoning 包含在 output 总量中。
- 表中误差为平均绝对百分比误差：逐条计算 `abs(预测−实际)/实际` 后取平均，不是最大误差，也不是所有任务的误差上限。
- decision 为预测加未测部分的15%余量。覆盖指 `decision >= 实际输入`，与请求是否成功、是否越过物理窗口是不同指标。
- 使用现有评估入口。46次生成请求之外有6次模型目录查询；额外计数请求为0，未加载tokenizer。所有发送内容均为合成数据。

## 精度结果

| 模型 / 模式 | 首次请求误差（3次） | 校准后追加误差（3次） | decision覆盖（6次） |
| --- | ---: | ---: | ---: |
| DeepSeek / non-thinking | 17.18% | 1.19% | 4/6 |
| DeepSeek / thinking | 16.93% | 1.18% | 4/6 |
| GPT-6 Luna / none | 4.86% | 0.31% | 6/6 |
| GPT-6 Luna / high | 4.86% | 0.30% | 6/6 |

本轮共12对精度请求：两模型 × 两模式 × 三语料；每对两次请求，共24次生成。全部追加请求均从JSONL恢复匹配的usage基线。
两种DeepSeek模式的代码/JSON首轮在加余量后分别仍少393、418 tokens；校准后两者仍各少14 tokens。OpenAI两种精度模式的12次decision均覆盖实际输入。

## 工具调用与连续压缩

两模型各完成7次主模型请求、4次摘要请求、4次压缩检查点。每组25/25项功能检查通过，包括用户约束、最终字段、历史搜索/读取、原始工具结果保存和运行时计量关联。
以下误差只针对有发送前计量快照的7次主请求；摘要请求不混入预测误差统计。

| 模型 | raw误差 | usage校准后的预测误差 | decision覆盖 | 最大decision缺口 |
| --- | ---: | ---: | ---: | ---: |
| DeepSeek | 20.04% | 17.69% | 0/7 | 974 tokens |
| GPT-6 Luna | 16.86% | 14.87% | 2/7 | 666 tokens |

### DeepSeek 主请求明细

| 请求 | 来源 | 预测 | 加余量后decision | 实际输入 |
| ---: | --- | ---: | ---: | ---: |
| 1 | estimated | 394 | 454 | 569 |
| 2 | estimated | 7069 | 8130 | 8764 |
| 3 | estimated | 7797 | 8967 | 9445 |
| 4 | estimated | 7889 | 9073 | 9536 |
| 5 | estimated | 7949 | 9142 | 9594 |
| 6 | provider_calibrated | 9965 | 10021 | 10028 |
| 7 | estimated | 8106 | 9322 | 10296 |

### GPT-6 Luna 主请求明细

| 请求 | 来源 | 预测 | 加余量后decision | 实际输入 |
| ---: | --- | ---: | ---: | ---: |
| 1 | estimated | 394 | 454 | 315 |
| 2 | estimated | 7108 | 8175 | 8446 |
| 3 | estimated | 7656 | 8805 | 8949 |
| 4 | estimated | 7617 | 8760 | 8915 |
| 5 | estimated | 7646 | 8793 | 8951 |
| 6 | provider_calibrated | 9332 | 9390 | 9375 |
| 7 | estimated | 7738 | 8899 | 9565 |

第2次请求的工具集合发生变化；第3、4、5、7次请求使用了新摘要。原usage基线不再匹配，因此回退为本地估算。只有第6次能复用基线。DeepSeek第6次加余量后仍少7 tokens，所以本轮覆盖是0/7，不能沿用旧报告的1/7。
最大百分比低估与最大token缺口可能来自不同请求。DeepSeek第1次实际569、decision454，低估20.21%；绝对缺口最大的第7次实际10,296、decision9,322，少974。OpenAI第7次实际9,565、decision8,899，少666，低估6.96%。

## 与旧本地估算结果比较及限制

工具与压缩场景预测误差：DeepSeek由旧测试17.76%变为本次17.69%；OpenAI由14.71%变为14.87%。该差异很小，生成的摘要和思考长度存在变化，不是严格同请求A/B，不能据此宣称精度改进或退化。
这次简化移除了额外请求和依赖，没有解决无基线时的估算误差。保留usage校准是有效的，但不能把稳定追加时的约1.2%/0.3%推广到频繁压缩任务。
运行时测试主动缩小本地窗口至32,768、压缩触发9,000、目标7,500、近期历史预算6,500，以有限调用触发压缩；真实服务端窗口仍约1M。本轮输入远低于服务端窗口，没有验证真实超窗恢复、1M边界、多模态或长任务可靠性。
“功能通过”表示任务与持久化契约通过，不表示精度或15%余量验收通过。本轮不调整生产参数。

## 用量与复现证据

共46次真实生成，无请求错误：输入362,052 tokens，输出10,204 tokens。输入中的缓存子集为150,844，输出中的reasoning子集为4,140，均未额外相加。
测试结束后携带凭证的运行进程已退出。报告仅保留数值、合成任务结果和计数元数据，不保存密钥或原始思考内容。

- [独立重算汇总](results/local-usage-20261021-rerun-summary.json)
- [被测源码与二进制SHA-256](results/local-usage-20261021-rerun-source.json)：测试前后核对一致，基于未提交工作区，不能仅用HEAD代表本轮代码。
- [deepseek-context-precision-local-usage-20261021-rerun.json](results/deepseek-context-precision-local-usage-20261021-rerun.json)
- [deepseek-context-thinking-precision-local-usage-20261021-rerun.json](results/deepseek-context-thinking-precision-local-usage-20261021-rerun.json)
- [deepseek-context-thinking-runtime-local-usage-20261021-rerun.json](results/deepseek-context-thinking-runtime-local-usage-20261021-rerun.json)
- [openai-context-none-precision-local-usage-20261021-rerun.json](results/openai-context-none-precision-local-usage-20261021-rerun.json)
- [openai-context-high-precision-local-usage-20261021-rerun.json](results/openai-context-high-precision-local-usage-20261021-rerun.json)
- [openai-context-high-runtime-local-usage-20261021-rerun.json](results/openai-context-high-runtime-local-usage-20261021-rerun.json)

复现时通过环境提供密钥，执行以下命令；应使用新label，避免覆盖本轮证据：

```sh
cargo run -p agent-runtime --example deepseek_context --locked -- precision non-thinking NEW-LABEL
cargo run -p agent-runtime --example deepseek_context --locked -- precision thinking NEW-LABEL
cargo run -p agent-runtime --example deepseek_context --locked -- runtime thinking NEW-LABEL
cargo run -p agent-runtime --example openai_context --locked -- precision none NEW-LABEL
cargo run -p agent-runtime --example openai_context --locked -- precision high NEW-LABEL
cargo run -p agent-runtime --example openai_context --locked -- runtime high NEW-LABEL
```
