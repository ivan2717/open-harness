# 成本优化的同条件质量复核

目标是在相同固定业务题、模型和测试条件下，比较功能完整的实际交付用量。不要修改业务提示、无关归档、模型、窗口、输出上限、预算或评分，以制造用量下降。

本轮基线为 `../results/collaboration-continuity-20261027-functional-450k/`：原评分 16/16，377,225 个已观测 tokens，182,004 ms。基线的 `review.md` 记录 gpt-6-luna / Responses / medium、24,576 窗口、4,096 最大输出、480 秒和 450,000 已观测 token 护栏；旧 `metadata.json` 未保存 reasoning 配置，自动比较必须标为未验证，不能补写历史元数据。

`continuity-audit.mjs` 对保存结果做只读复核，不修改原分、不修正产物、不再执行模型脚本、不请求模型 API。它要求原评分全部通过，再独立重算四份来源，并核验以下证据：

- 子任务确实读取父会话原始要求和更正全文，绑定真实父 ID 和消息 ID。
- 主子任务都有完整来源读取结果，来源与固定输入逐字一致。
- 子任务成功创建临时脚本，然后由自己的宿主命令实际执行；命令 ID、成功工具返回和保存脚本相符。
- 最终文件包含更正的准确短引文、来源文件和关键记录 ID，主任务实际读回的 JSON 与保存产物一致。
- 唯一子任务及主任务均正常结束，工作区 review 完整，无 busy、未决审批或遗留命令。

完整原文预览直接满足读取要求：`agent_control read` 的对应消息须为 `status=complete` 且 `truncated=false`，不要求再次精读；搜索预览则须为绑定父会话和用户字段的完整 `preview_truncated=false` 结果。全文分段读取按 Unicode 字符偏移拼接，只有从 0 到完整长度均被真实返回才通过；最后一页的 `truncated=false` 本身不代表整条消息已读。不会为冗余回读增加质量分。

这些门槛是保守的证据识别器。复杂脚本启动方式、仅通过脚本读取材料等无法自动证明的情形会失败，不能静默改为通过。脚本是否根据来源计算而非硬编码，主任务是否确实核验，以及依据文字是否准确，仍须阅读公开脚本与消息复核。不要为得到更好分数在模型运行后补写文件或补跑其脚本。

```sh
node --test evals/collaboration/continuity-evidence.test.mjs evals/collaboration/continuity-audit.test.mjs
node evals/collaboration/continuity-audit.mjs evals/results/collaboration-continuity-20261027-functional-450k
node evals/collaboration/continuity-compare.mjs BASELINE_DIRECTORY FIRST_NEW_RUN_DIRECTORY SECOND_NEW_RUN_DIRECTORY
```

审计及比较报告输出为标准输出 JSON，可保存到新的审核文件；不能覆盖 `result.json`、`transcripts.json` 或模型工作区。比较器核对原检查项、完整提示词摘要、来源摘要、模型、协议、窗口、输出上限与预算，分别列出质量、条件一致性、主子用量和总体用量。缺失历史 reasoning 元数据会单独标注；真实测试时仍须核对 launcher 显示的 medium 配置。

真实运行继续使用 `live-electron.cjs` 的保险库只读内存解密路径，复用保存的模型配置。只读取既有保险库，不输出密钥或整个配置，不扫描额外私有目录。由主任务确认 API 构建完成后才启动，每次使用新输出目录和隔离状态。运行结束确认 API 退出、临时私有状态删除，再保存公开证据审查结论。

两次新运行与一次基线只支持这个固定用例的描述性比较，不支持统计显著性或普遍成功率。已观测 tokens 不等于账单，也可能遗漏失败调用、重试、压缩及 Wiki 用量。未完成、质量检查失败或条件不同的运行须保留原结果，不能把其 token 减少称为同质量节省。
