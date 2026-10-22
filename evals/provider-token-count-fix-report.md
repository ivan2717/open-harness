# 请求计数修复与真实模型复测

> 历史实验记录：下述 tokenizer 和 OpenAI 额外计数请求方案已于 2026-10-21
> 撤回。当前实现统一本地估算与正常响应的 usage 校准，不再包含相关计数接口、
> 缓存或安装脚本。以下数据保留原实验口径，不能作为当前实现的精度结论；
> 原方案已不能通过当前 example 复现。

日期：2026-10-21。修复针对上一轮工具 schema 变化、首次工具输出和压缩后
重建上下文时的低估；保留上一轮原始报告，本轮使用 `provider-count-20261021`
标签，不覆盖历史证据。

**结果：本轮 39 条有请求前计量快照的真实请求，预测输入数均与服务端实际输入
一致。两组 runtime 各 25/25 项功能检查通过。这个结果仅覆盖本次文本样本，
不能推导所有模型、图片或近 1M 输入都没有误差。**

## 修改内容

核心新增可选异步 `TokenCounter::count_input_tokens`，由适配器解释真实请求
格式。执行循环仍不需要知道 Chat Completions、Responses 或其他协议。

- OpenAI 使用实际编码的请求调用 `/responses/input_tokens`，保留 instructions、
  input、tools 和 reasoning 设置，仅投影该接口支持的字段。完整服务端计数
  标为 `provider_counted`，不再叠加估算余量。
- DeepSeek 使用固定 1017 官方 encoder 和 tokenizer，计入模型分隔符、DSML
  工具说明、工具结果及对应思考保留规则。固定提交、文件 SHA-256、Python
  依赖版本和 wheel hashes；资产只缓存在忽略的 `target/`。因托管别名可能
  升级，标为 `tokenizer_estimated`，继续保留 15% 估算余量。
- 普通和原生 context manager 在初始请求、压缩候选、fallback 上使用新计数；
  最终预算判断和诊断读取同一结果。主模型和摘要模型分别计量。
- 成功计数缓存上限为 128，绑定完整请求和模型身份。工具、输出上限、消息或
  opaque 状态变化会失效。缓存不另存提示文本。不可用或失败返回 `None`，
  保留原有 usage 前缀校准及粗估回退；显式零计数与失败严格区分。
- 摘要先计数完整批次；超限再二分寻找已验证能容纳的前缀，避免每追加一个
  event 就发一次远程计数。发送摘要前仍检查其独立预算。
- 计数支持超时、取消及子进程清理；本地 tokenizer 子进程不继承密钥环境。
  本地总量小于已测 native 组件时拒绝该结果，避免遗漏 opaque 成本。
- API 与桌面展示新增两个计量来源。桌面目前仍是离线 DemoModel；真实 provider
  transport 仍位于 opt-in examples，本次没有另行添加桌面云模型配置。

## 精度结果

MAPE 是各请求绝对百分比误差的平均。prediction 在请求前获得，再与该次返回的
实际 input usage 比较；decision 另含估算余量，不将这部分余量算成预测误差。

| 模型 / 场景 | 上轮 prediction MAPE | 本轮 prediction MAPE | 本轮计量请求 | decision 覆盖 |
| --- | ---: | ---: | ---: | ---: |
| DeepSeek 普通模式，JSONL 恢复追加 | 1.19% | 0% | 3 | 3/3 |
| DeepSeek 思考模式，JSONL 恢复追加 | 1.18% | 0% | 3 | 3/3 |
| DeepSeek 思考模式，工具与连续压缩 | 17.76% | 0% | 8 | 8/8 |
| GPT-6 Luna 普通模式，JSONL 恢复追加 | 0.31% | 0% | 3 | 3/3 |
| GPT-6 Luna 思考模式，JSONL 恢复追加 | 0.30% | 0% | 3 | 3/3 |
| GPT-6 Luna 思考模式，工具与连续压缩 | 14.71% | 0% | 7 | 7/7 |

此外，两家各 6 条冷启动请求也全部零误差。总计 24 条 precision 请求和 15 条
runtime 主请求，全部使用新计数来源。precision 仍分别使用英文、中文、代码/JSON
三种约 30 KB 合成语料；恢复后先独立验证 `provider_calibrated` 基线，再应用
完整请求计数，避免新计数覆盖后掩盖 JSONL 恢复退步。

上轮与本轮 runtime 的场景、工具和预算相同，但模型生成的摘要、工具参数和
思考内容不同。这是同场景复测，并非每条请求内容完全相同的 A/B。
本轮同一请求保留的 raw 粗估 MAPE 仍为 DeepSeek 19.93%、OpenAI 16.63%，
新计数的改善不是由把旧粗估数字重新命名或增大余量得到的。

## 连续工具与压缩

| 指标 | DeepSeek Flash / high | GPT-6 Luna / high |
| --- | ---: | ---: |
| 主模型生成请求 | 8 | 7 |
| 摘要生成请求 | 5 | 4 |
| 保存的压缩检查点 | 4 | 4 |
| 功能检查 | 25/25 | 25/25 |
| runtime 最大实际输入（含摘要） | 10,297 | 9,620 |

两组均完成四轮业务工具、历史搜索与读取、被裁剪事实恢复和最终字段检查；
用户约束完整保留，canonical 事件没有被压缩修改。DeepSeek 首次历史搜索的
`limit=10` 超过工具上限 8，工具返回错误后模型改为 8，最终成功。因此它比
OpenAI 多一次主请求；不能把两组生成次数或总用量直接作为效率排名。

DeepSeek 5 次摘要生成仅形成 4 个保存检查点，其中一次候选没有缩小上下文而被
弃用。两组均未达到测试配置的 7,500 软目标：DeepSeek 检查点 decision 为
10,856、10,830、10,861、11,712；OpenAI 为 8,900、8,903、8,876、9,487。
它们均降低了上下文且满足输入硬预算，但保留近期工具步骤会限制可压缩空间。
这次修复计量准确性，不宣称每次压缩都能达到软目标。

本轮共 9 条摘要生成没有在 runtime 报告中关联独立的 measurement 快照，因此
“39 条零误差”不包括它们。摘要预计数接线、发送前预算拒绝、缓存复用与分批
正确性另有离线回归测试。

## 用量与计数成本

| 模型 | 生成请求 | 输入 tokens | 输出 tokens | 缓存输入子集 | 思考输出子集 |
| --- | ---: | ---: | ---: | ---: | ---: |
| DeepSeek | 25 | 192,162 | 6,083 | 116,480 | 2,260 |
| OpenAI | 23 | 179,061 | 3,401 | 51,441 | 1,205 |

总计 48 次生成请求，不含 `/models` 查询。没有自动网络重试或失败后挑选重跑。
缓存和 reasoning 均已包含在相应输入、输出总量中，不再次相加。

OpenAI 额外发出 **27 次计数请求，27 次成功**：两组 precision 各 6 次，runtime
15 次。观察到的计数请求耗时合计 58,216 ms，平均约 2.16 秒，其中 runtime
30,912 ms。这是新增网络成本，不能当作免费或零延迟优化；计数请求与生成 usage
分开报告。DeepSeek 使用本地 tokenizer，不发远程计数请求。

所有模型输入均为合成数据，没有上传仓库文件。密钥只保留在临时进程环境中，
进程已退出。新增报告不保存原始思考文本或加密思考 payload；改动文件密钥
模式扫描未发现命中。

## 验证与边界

- `cargo test --workspace --all-targets --locked --offline`：323 passed，0 failed；
  1 个需要本地官方资产的测试默认忽略，已单独运行通过。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 通过。
- Rust 格式与 diff 空白检查通过；桌面 typecheck、12 项测试和相关格式检查通过。
- 新测试覆盖完整计数压过错误粗估、压缩后重计、模型/schema/output cap 缓存
  隔离、None/零区别、native 不双算、摘要硬预算及取消后子进程退出。

本轮不覆盖真实 1M 边界、多模态、其他 provider 的 tokenizer、服务端原生压缩
或服务端主动超窗恢复。DeepSeek 缺失资产或不支持输入类型时仍明确回退估算；
OpenAI 计数服务不可用时同样回退。现有超窗恢复路径保留，但不能用本轮小样本
成功来证明任何 fallback 都不会低估。

## 证据与复现

- [DeepSeek 普通精度](results/deepseek-context-precision-provider-count-20261021.json)
- [DeepSeek 思考精度](results/deepseek-context-thinking-precision-provider-count-20261021.json)
- [DeepSeek 思考 runtime](results/deepseek-context-thinking-runtime-provider-count-20261021.json)
- [OpenAI 普通精度](results/openai-context-none-precision-provider-count-20261021.json)
- [OpenAI 思考精度](results/openai-context-high-precision-provider-count-20261021.json)
- [OpenAI 思考 runtime](results/openai-context-high-runtime-provider-count-20261021.json)
- 原 DeepSeek tokenizer 安装说明及脚本已随方案移除；固定来源见下方协议依据。
- [OpenAI 运行说明](../crates/agent-runtime/examples/openai_context/README.md)
- [上轮 DeepSeek 报告](deepseek-token-measurement-report.md)
- [上轮 OpenAI 报告](openai-token-measurement-report.md)

协议依据：[OpenAI input token counting](https://developers.openai.com/api/docs/guides/token-counting)、
[DeepSeek 1017 官方 encoder](https://huggingface.co/deepseek-ai/DeepSeek-V4.1-Flash-1017/blob/1bae0e1088993b9b82de08f59de5be5a34e42170/encoding/README.md)。
