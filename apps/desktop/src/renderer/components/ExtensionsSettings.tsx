import { useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ChevronDown,
  FolderOpen,
  LoaderCircle,
  Pencil,
  Plug,
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import type { McpServer } from '../../shared/extensions'
import { extensionSettingsOptions, useSaveExtensions, withSkillEnabled } from '../lib/extensions'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import { McpServerEditor } from './McpServerEditor'
import { SkillDiagnostics, SkillList } from './SkillList'
import '../extensions.css'

export function ExtensionsSettings() {
  const query = useQuery(extensionSettingsOptions)
  const save = useSaveExtensions()
  const [editing, setEditing] = useState<string | null>(null)
  const [localError, setLocalError] = useState('')
  const openDirectory = useMutation({ mutationFn: () => window.harness.openSkillsDirectory() })
  const config = query.data?.config
  function saveServer(server: McpServer) {
    setLocalError('')
    if (editing === '' && config?.mcp_servers.some((entry) => entry.id === server.id)) {
      setLocalError('这个标识已经在使用，请换一个。')
      return
    }
    save.mutate(
      (current) => {
        if (editing === '' && current.mcp_servers.some((entry) => entry.id === server.id)) {
          throw new Error('这个标识已经在使用，请换一个。')
        }
        return {
          ...current,
          mcp_servers: [...current.mcp_servers.filter((entry) => entry.id !== server.id), server],
        }
      },
      { onSuccess: () => setEditing(null) },
    )
  }
  function editServer(id: string | null) {
    setEditing(id)
    setLocalError('')
    save.reset()
  }
  return (
    <>
      <section className="settings-section extension-settings">
        <div className="extension-section-heading">
          <div>
            <h2>工具服务</h2>
            <p>连接 MCP 服务，为 Agent 添加外部工具。</p>
          </div>
          <button
            type="button"
            className="extension-text-button"
            disabled={!config || save.isPending}
            onClick={() => editServer('')}
          >
            <Plus size={15} />
            添加服务
          </button>
        </div>
        {query.isPending && <p className="extension-hint">正在读取配置…</p>}
        {query.isError && (
          <InlineError message={errorMessage(query.error)} retry={() => void query.refetch()} />
        )}
        {config?.mcp_servers.length === 0 && editing !== '' && (
          <p className="extension-empty">尚未连接工具服务。</p>
        )}
        <div className="extension-list">
          {config?.mcp_servers.map((server) => (
            <McpServerRow
              key={server.id}
              server={server}
              busy={save.isPending}
              onEdit={() => editServer(server.id)}
              onToggle={() =>
                save.mutate((current) => ({
                  ...current,
                  mcp_servers: current.mcp_servers.map((entry) =>
                    entry.id === server.id ? { ...entry, enabled: !entry.enabled } : entry,
                  ),
                }))
              }
              onDelete={() =>
                save.mutate(
                  (current) => ({
                    ...current,
                    mcp_servers: current.mcp_servers.filter((entry) => entry.id !== server.id),
                  }),
                  {
                    onSuccess: () => {
                      if (editing === server.id) setEditing(null)
                    },
                  },
                )
              }
            />
          ))}
        </div>
        {editing !== null && config && (
          <McpServerEditor
            key={editing}
            server={config.mcp_servers.find((entry) => entry.id === editing)}
            busy={save.isPending}
            error={localError || (save.isError ? errorMessage(save.error) : undefined)}
            onSave={saveServer}
            onCancel={() => editServer(null)}
          />
        )}
        {editing === null && save.isError && <InlineError message={errorMessage(save.error)} />}
        <p className="settings-footnote">
          启用后，Agent 可连接服务并发现工具。MCP
          服务使用自身权限，可能联网或访问项目外的文件；工具调用会单独请求批准。
        </p>
      </section>
      <section className="settings-section extension-settings">
        <div className="extension-section-heading">
          <div>
            <h2>技能</h2>
            <p>按任务加载工作流程，支持 SKILL.md 格式。</p>
          </div>
          <div className="extension-actions">
            <button
              type="button"
              className="extension-text-button"
              disabled={openDirectory.isPending || !query.data}
              onClick={() => openDirectory.mutate()}
            >
              <FolderOpen size={15} />
              打开文件夹
            </button>
            <button
              type="button"
              className="extension-icon-button"
              aria-label="刷新技能"
              title="刷新技能"
              disabled={query.isFetching}
              onClick={() => void query.refetch()}
            >
              <RefreshCw size={15} className={query.isFetching ? 'animate-spin' : undefined} />
            </button>
          </div>
        </div>
        {query.data && (
          <SkillList
            skills={query.data.skills}
            busy={save.isPending}
            onToggle={(id, enabled) =>
              save.mutate((current) => withSkillEnabled(current, id, enabled))
            }
          />
        )}
        {query.data?.skills.length === 0 && (
          <p className="extension-empty">
            尚未安装个人技能。将包含 SKILL.md 的技能文件夹放入此目录，再刷新即可。
          </p>
        )}
        <details className="extension-install-help">
          <summary>
            <ChevronDown size={13} />
            安装与使用
          </summary>
          {query.data && <code>{query.data.skills_directory}</code>}
          <p>
            每个技能使用独立文件夹，SKILL.md 开头的 YAML 填写 name 和
            description，其后编写操作说明；可附带 references、scripts 等资源。
          </p>
          <p>
            项目中的 .agents/skills 和 .openharness/skills
            会自动发现，在会话的「技能」入口查看。同名技能优先使用项目版本，开关作用于同名技能。
          </p>
          <p>
            在消息中输入 $技能名称可指定使用，也可由 Agent
            按描述选择。技能只提供工作流程，读取脚本不会自动执行。
          </p>
        </details>
        {query.data && <SkillDiagnostics diagnostics={query.data.diagnostics} />}
        {openDirectory.isError && <InlineError message={errorMessage(openDirectory.error)} />}
      </section>
    </>
  )
}

function McpServerRow({
  server,
  busy,
  onEdit,
  onToggle,
  onDelete,
}: {
  server: McpServer
  busy: boolean
  onEdit: () => void
  onToggle: () => void
  onDelete: () => void
}) {
  const [confirmDelete, setConfirmDelete] = useState(false)
  const test = useMutation({ mutationFn: () => window.harness.testMcp(server.id) })
  const title = server.name || server.id
  return (
    <div className="mcp-server">
      <div className="extension-row">
        <Plug size={16} className="extension-row-icon" />
        <div className="extension-identity">
          <strong>{title}</strong>
          <span>
            {server.transport.type === 'stdio' ? '本地命令' : '远程服务'} ·{' '}
            {server.enabled ? '已启用' : '已关闭'}
          </span>
        </div>
        <div className="extension-actions">
          <button
            type="button"
            className="extension-text-button"
            disabled={busy || test.isPending || !server.enabled}
            title={server.enabled ? '连接服务并读取工具列表' : '启用后可测试连接'}
            onClick={() => test.mutate()}
          >
            {test.isPending ? <LoaderCircle size={14} className="animate-spin" /> : null}
            {test.isPending ? '连接中' : '测试连接'}
          </button>
          <button
            type="button"
            className="extension-icon-button"
            aria-label={`编辑 ${title}`}
            title="编辑服务"
            disabled={busy || test.isPending}
            onClick={onEdit}
          >
            <Pencil size={14} />
          </button>
          <button
            type="button"
            className="extension-icon-button"
            aria-label={`删除 ${title}`}
            title="删除服务"
            disabled={busy || test.isPending}
            onClick={() => setConfirmDelete(!confirmDelete)}
          >
            <Trash2 size={14} />
          </button>
          <button
            type="button"
            role="switch"
            aria-checked={server.enabled}
            aria-label={`启用服务 ${title}`}
            className={`toggle-switch ${server.enabled ? 'is-on' : ''}`}
            disabled={busy || test.isPending}
            onClick={() => {
              test.reset()
              onToggle()
            }}
          >
            <span />
          </button>
        </div>
      </div>
      {confirmDelete && (
        <div className="extension-delete-confirm">
          <span>移除 {title} 的连接配置？</span>
          <button
            type="button"
            className="extension-text-button"
            disabled={busy}
            onClick={onDelete}
          >
            移除
          </button>
          <button
            type="button"
            className="extension-text-button"
            disabled={busy}
            onClick={() => setConfirmDelete(false)}
          >
            取消
          </button>
        </div>
      )}
      {test.isError && <InlineError message={errorMessage(test.error)} />}
      {test.isSuccess && (
        <details className="extension-test-result">
          <summary>
            <ChevronDown size={13} />
            已连接 · {test.data.tools.length} 个工具
          </summary>
          {test.data.tools.length > 0 ? (
            <ul>
              {test.data.tools.map((tool) => (
                <li key={tool.name}>
                  <strong>{tool.name}</strong>
                  {tool.description && <p>{tool.description}</p>}
                </li>
              ))}
            </ul>
          ) : (
            <p>此服务未提供工具。</p>
          )}
        </details>
      )}
    </div>
  )
}
