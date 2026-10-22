import { Fragment, useState, type FormEvent } from 'react'
import type { McpServer } from '../../shared/extensions'
import { InlineError } from './Feedback'

interface Props {
  server?: McpServer
  busy: boolean
  error?: string
  onSave: (server: McpServer) => void
  onCancel: () => void
}

export function McpServerEditor({ server, busy, error, onSave, onCancel }: Props) {
  const [transport, setTransport] = useState<'stdio' | 'http'>(server?.transport.type ?? 'stdio')
  const [validation, setValidation] = useState('')
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setValidation('')
    const data = new FormData(event.currentTarget)
    const field = (name: string) => String(data.get(name) ?? '').trim()
    const id = field('id')
    const envVars = field('env_vars')
      .split(/[\s,]+/)
      .filter(Boolean)
    const tokenEnv = field('bearer_token_env')
    if (
      [...envVars, ...(tokenEnv ? [tokenEnv] : [])].some(
        (name) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name),
      )
    ) {
      setValidation('环境变量请填写名称，例如 API_KEY；不要填写密钥本身。')
      return
    }
    const url = field('url')
    if (transport === 'http') {
      let parsed: URL
      try {
        parsed = new URL(url)
      } catch {
        setValidation('请填写完整的服务地址。')
        return
      }
      const localHttp =
        parsed.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(parsed.hostname)
      if (
        (parsed.protocol !== 'https:' && !localHttp) ||
        parsed.username ||
        parsed.password ||
        parsed.hash
      ) {
        setValidation(
          '请使用 HTTPS 或本机 HTTP 地址，不含账户、密码或片段；身份凭据通过环境变量提供。',
        )
        return
      }
    }
    onSave({
      id,
      name: field('name') || id,
      enabled: server?.enabled ?? true,
      transport:
        transport === 'stdio'
          ? {
              type: 'stdio',
              command: field('command'),
              args: String(data.get('args') ?? '')
                .split(/\r?\n/)
                .filter((line) => line.trim() !== ''),
              env_vars: [...new Set(envVars)],
            }
          : { type: 'http', url, bearer_token_env: tokenEnv || null },
    })
  }
  return (
    <form className="model-form extension-editor" onSubmit={submit}>
      <fieldset disabled={busy}>
        <div className="model-form-row">
          <label>
            标识
            <input
              name="id"
              required
              pattern="[a-zA-Z0-9_-]+"
              maxLength={64}
              defaultValue={server?.id ?? ''}
              readOnly={Boolean(server)}
              placeholder="例如 documents"
            />
          </label>
          <label>
            显示名称
            <input
              name="name"
              maxLength={128}
              defaultValue={server?.name ?? ''}
              placeholder="例如 文档工具"
            />
          </label>
        </div>
        <label>
          连接方式
          <select
            value={transport}
            onChange={(event) => {
              setTransport(event.target.value as 'stdio' | 'http')
              setValidation('')
            }}
          >
            <option value="stdio">本地命令（stdio）</option>
            <option value="http">远程服务（HTTP）</option>
          </select>
        </label>
        {transport === 'stdio' ? (
          <Fragment key="stdio">
            <label>
              启动命令
              <input
                name="command"
                required
                maxLength={2048}
                defaultValue={server?.transport.type === 'stdio' ? server.transport.command : ''}
                placeholder="例如 npx 或可执行文件的绝对路径"
                autoComplete="off"
              />
            </label>
            <label>
              参数 · 每行一个
              <textarea
                name="args"
                rows={3}
                defaultValue={
                  server?.transport.type === 'stdio' ? server.transport.args.join('\n') : ''
                }
                placeholder={'-y\n服务包名称'}
                spellCheck={false}
              />
            </label>
            <label>
              传入的环境变量名称 · 可选
              <textarea
                name="env_vars"
                rows={2}
                defaultValue={
                  server?.transport.type === 'stdio' ? server.transport.env_vars.join('\n') : ''
                }
                placeholder="例如 API_KEY，每行一个名称"
                spellCheck={false}
              />
            </label>
          </Fragment>
        ) : (
          <Fragment key="http">
            <label>
              服务地址
              <input
                name="url"
                type="url"
                required
                defaultValue={server?.transport.type === 'http' ? server.transport.url : ''}
                placeholder="https://example.com/mcp"
                autoComplete="off"
              />
            </label>
            <label>
              令牌所在的环境变量名称 · 可选
              <input
                name="bearer_token_env"
                defaultValue={
                  server?.transport.type === 'http' ? (server.transport.bearer_token_env ?? '') : ''
                }
                placeholder="例如 MCP_API_TOKEN"
                autoComplete="off"
                spellCheck={false}
              />
            </label>
          </Fragment>
        )}
        <p className="extension-hint">
          环境变量从启动 Open Harness 的进程读取。密钥请放在环境变量中，避免写入地址或参数。
        </p>
        <div className="extension-actions">
          <button type="submit" className="button button-primary">
            {busy ? '正在保存…' : '保存服务'}
          </button>
          <button
            type="button"
            className="button button-secondary"
            disabled={busy}
            onClick={onCancel}
          >
            取消
          </button>
        </div>
      </fieldset>
      {(validation || error) && <InlineError message={validation || error!} />}
    </form>
  )
}
