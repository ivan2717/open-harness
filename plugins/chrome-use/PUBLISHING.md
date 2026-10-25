# Chrome Web Store 发布准备

当前状态：**尚未创建或发布 Chrome Web Store 条目**。
`distribution.json` 的 `chromeWebStoreUrl` 保持 `null`。打包成功只说明本地
ZIP 已生成并通过检查，不代表已上传、已审核或可以从商店安装。

## 本地打包

需要 Node.js 22 和 `zip` 命令。项目根目录执行：

```sh
node plugins/chrome-use/scripts/package-extension.mjs
node --test plugins/chrome-use/tests/package.test.mjs
```

默认产物：`plugins/chrome-use/dist/open-harness-chrome-0.1.0.zip`。
也可以使用 `--out /absolute/path/extension.zip` 指定输出。输出 JSON 包含文件清单、
版本、SHA-256 和发行 URL 配置状态。`dist/` 已忽略，不把二进制产物当作源码提交。

ZIP 根目录是 `manifest.json`，仅包含明确允许的扩展运行文件和 16、32、48、128 像素
PNG 图标。它不包含 Native Messaging host、MCP server、skills、测试、开发脚本、
`distribution.json`、本地配置、认证 token、私钥或源码中的 `manifest.key`。
公开 key 的省略是为了把商店签名与本地开发 ID 分开；它本身不是秘密。
新增依赖、权限或入口应先更新打包检查和相应发布说明，不要把整个插件目录直接压缩。
图标的可编辑源在 `assets/icon.svg`；`scripts/icons.mjs` 可再生成各尺寸 PNG。

## 首次上传后的 ID 同步

现有 `keibjmjjimfgkikahpjgbfebiglbjkee` 是根据仓库中公开 key 确定的**开发扩展 ID**。
它不是已经存在的商店 Item ID；本地 `key` 不构成对商店 ID 的预留。本仓库没有保留
对应私钥，也不应尝试用未知私钥证明旧 ID 的所有权。

按 Chrome 官方的 ID 一致性流程操作：

1. 由项目所有者准备 Chrome Web Store 开发者账号和所需发布权限，上传生成的 ZIP
   **创建草稿，先不要提交发布**。本脚本不登录、不上传、不提交审核。
2. 在开发者后台记录真实 **Item ID**，从 **Package → View public key** 获取公开 key。
   去除 PEM 首尾标记和换行，只把 Base64 的公钥内容放进开发源码 `extension/manifest.json`
   的 `key` 字段。不要把任何私钥、OAuth token 或发布者凭据放进仓库。
3. 同步这四处，并在同一版本中交付：
   - `extension/manifest.json` 的公开 key；
   - `constants.mjs` 的 `EXTENSION_ID`；
   - 桌面 Chrome 连接模块中的固定扩展 ID 及其测试；
   - 相关测试、文档中明确写出的开发 ID。
   本地 key 派生的 ID、桌面预期 ID、商店 Item ID 必须一致。打包脚本会拒绝前两项不一致。
4. 重新构建桌面应用，更新 host 注册：`allowed_origins` 只允许当前真实扩展 origin。
   已保存的连接配置也需要桌面「启用并修复连接」流程重新准备。旧开发扩展先断开，避免它与
   新商店扩展争用同一个 host；开发与商店版本不能各自假装拥有同一个运行会话。
5. 重新生成 ZIP，按开发者后台要求递增 `manifest.version` 并更新同一个草稿条目。
   ZIP 仍省略开发 `key`；商店对同一 Item 的更新保持该 Item 身份。以后只更新该条目，
   不要每次发布创建新 Item。
6. 在隔离的开发 profile 验证 key 派生 ID，再通过商店的测试分发能力验证真实安装：
   Native Messaging 连接、用户确认权限、后台新建/读取/输入/截图、退出清理和跨站重新授权。
   不需要把「加载已解压的扩展程序」交给正式用户。

依据：[Chrome 官方 manifest key 说明](https://developer.chrome.com/docs/extensions/reference/manifest/key)
与[商店发布流程](https://developer.chrome.com/docs/webstore/publish)。

## 正式发行 URL 契约

`distribution.json` 只包含一个字段：

```json
{ "chromeWebStoreUrl": null }
```

获得**已验证可访问、可供目标用户安装的真实商店条目**后，所有者再填入它的 HTTPS URL。
允许的地址结构是 Chrome Web Store 的 `/detail/<Item ID>` 或
`/detail/<条目名称>/<Item ID>`；域名必须是 `chromewebstore.google.com`，末尾 ID 必须
与 `EXTENSION_ID` 一致。不接受登录凭据、查询参数、片段、其他域名或重定向链接。
不要根据当前开发 ID 拼一个地址当成已经上架。

桌面的一键连接入口负责准备本机 host、启用内置插件、打开已配置的商店页面和检查连接。
用户仍需在 Chrome 确认 **添加扩展程序**；安装后还需明确允许 Agent 新建后台页或共享当前页。
这个流程不能绕过 Chrome 的确认。配置为 `null` 时应明确说明商店安装尚未开放，开发安装
只能作为可展开的开发选项，不能伪装成正式一键安装。

## 提交审核之前要准备的内容

以下文件和素材是待审核的发布准备，不自动构成已完成的商店条目：

- 商店名称、简短说明、详细说明，以及明确的单一用途。
- 隐私政策的真实公开 URL、开发者联系方式和支持入口。
- 每个权限的用途解释，包含 `debugger`、`tabs`、`tabGroups`、`storage`、`alarms`、`nativeMessaging`。
- 数据使用声明。不能勾选「完全不处理用户数据」：被授权页面的文字、网址、用户输入结果、
  截图会交给本机 Open Harness；桌面应用还可能把它们发送给用户选择的模型提供方并写入会话。
- 至少一张展示真实用户体验的商店截图。官方要求 1280×800 或 640×400；128×128 的扩展
  PNG 图标已经放入 ZIP，截图需要在最终连接流程中采集，不能以不存在的安装体验作图。
- 审核员可运行的桌面应用获取方式和测试说明。扩展依赖本机 host，必须明确该前提，提供
  能够验证实际功能的环境，而不是让审核员面对始终离线的弹窗。
- 所有者确认发布范围、审核后是否自动上线，再自行提交。审核通过和上架时间不作保证。

下面的说明可以作为后台文字的起点；提交者须核对最终应用和隐私政策：

**单一用途**：把用户授权的 Chrome 标签页连接到本机 Open Harness，让用户委托的 Agent
在后台标签页读取和操作网页，同时让用户继续使用自己的标签页。

**权限说明**：

| 权限 | 实际用途 |
| --- | --- |
| `debugger` | 按指定标签页调用 Chrome DevTools Protocol，读取 DOM、执行固定动作、抓取该标签页截图；不开放任意模型 JavaScript 或任意 CDP 方法。 |
| `tabs` | 查询标签页 ID、网址和标题，核验共享目标与 origin；未共享且非本会话创建的标签页不发送给 Agent。 |
| `tabGroups` | 为会话新建的后台页面创建和复用专用分组；不移动用户原有页面，不按整组关闭。 |
| `storage` | 本机保存后台标签页开关、主动断开连接的选择和重连退避状态；浏览器会话内缓存分组布局。既有标签页共享/会话控制权不跨重启恢复。 |
| `alarms` | 在扩展后台休眠后继续恢复意外断开的本地连接；用户主动断开时取消。不会重放浏览器操作或恢复旧授权。 |
| `nativeMessaging` | 与固定名称 `dev.openharness.chrome` 的本地 host 通信，连接本机桌面应用；不启动公网调试端口。 |

**数据处理说明**：扩展不直接调用模型服务或加入遥测，但会将任务所需、已授权的网页内容和
截图经本地连接交给 Open Harness。用户选择的模型提供方可能接收这些内容，本地会话记录
也可能保存它们。浏览器继续使用现有登录状态；扩展不读取或导出 cookie 数据库，不支持
填写密码输入框，不扫描浏览器历史记录。公开隐私政策还需要说明保存期限、删除方式、
联系人以及完整的数据接收方说明；不能仅用「全部本地」概括这一链路。

**代码执行说明**：所有执行代码随扩展打包。模型只传固定工具名和结构化参数；在页面隔离
世界执行的固定函数来自包内 `page.mjs`，不是模型生成的脚本或远程下载的程序。请在审核
说明中如实解释 `debugger` 和固定函数的用途；该说明不等于预先得到商店政策批准。

参考：[商店图片要求](https://developer.chrome.com/docs/webstore/images)、
[Chrome Web Store 用户数据政策](https://developer.chrome.com/docs/webstore/program-policies/policies)、
[Native Messaging 官方说明](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)。
