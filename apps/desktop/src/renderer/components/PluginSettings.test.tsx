import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it, vi } from 'vitest'
import type { BundledPlugin, ExtensionSettings, PluginInfo } from '../../shared/extensions'
import { PluginSettings } from './PluginSettings'

vi.mock('@tanstack/react-router', () => ({
  Link: ({ to, children, ...props }: any) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
}))

const chromeConnection: NonNullable<BundledPlugin['chrome']> = {
  configured: true,
  connected: true,
  allowNewTabs: true,
  storeUrl: null,
  extensionPath: '/plugins/chrome-use/extension',
}

const catalog: BundledPlugin[] = [
  {
    id: 'browser-use',
    name: '独立浏览器',
    description: '在独立浏览器中操作网页。',
    available: true,
    browserReady: true,
  },
  {
    id: 'computer-use',
    name: '电脑操作',
    description: '操作已授权的应用。',
    available: true,
    permissions: {
      accessibility: true,
      screenRecording: true,
      helperPath: '/plugins/computer-use/helper',
    },
  },
  {
    id: 'chrome-use',
    name: 'Chrome',
    description: '在 Chrome 后台标签页中操作。',
    available: true,
    chrome: chromeConnection,
  },
]

function plugin(id: string, name: string, overrides: Partial<PluginInfo> = {}): PluginInfo {
  return {
    id,
    name,
    version: '0.1.0',
    description: `${name}插件`,
    enabled: true,
    source_path: `/plugins/${id}`,
    content_hash: '0123456789abcdef',
    skill_count: 0,
    mcp_servers: [],
    warnings: [],
    ...overrides,
  }
}

const installed = [
  plugin('browser-use', '浏览器操作'),
  plugin('computer-use', '电脑操作'),
  plugin('chrome-use', 'Chrome'),
  plugin('third-party', '外部插件'),
]

function settings(plugins: PluginInfo[] = installed): ExtensionSettings {
  return {
    config: { mcp_servers: [], disabled_skill_ids: [] },
    builtin_plugins: [
      { id: 'builtin.llm-wiki', name: 'LLM Wiki', description: '会话记忆', enabled: true },
    ],
    plugins,
    skills_directory: '/skills',
    skills: [],
    diagnostics: [],
  }
}

function render(options: {
  settings?: ExtensionSettings
  catalog?: BundledPlugin[]
  loading?: boolean
  catalogError?: Error
}): string {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, retryOnMount: false } },
  })
  if (options.catalog !== undefined) {
    client.setQueryData(['bundled-plugins'], options.catalog)
  }
  if (options.catalogError) {
    client
      .getQueryCache()
      .build(client, { queryKey: ['bundled-plugins'] })
      .setState({
        error: options.catalogError,
        status: 'error',
        fetchStatus: 'idle',
      })
  }
  client.setQueryData(['chrome-connection'], chromeConnection)
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <PluginSettings
          settings={options.settings}
          loading={options.loading ?? false}
          error={null}
          retry={() => {}}
        />
      </QueryClientProvider>,
    )
  } finally {
    client.clear()
  }
}

function articles(html: string): string[] {
  return html.match(/<article\b[\s\S]*?<\/article>/g) ?? []
}

function article(html: string, name: string): string {
  const matches = articles(html).filter((item) => item.includes(`aria-label="插件 ${name}"`))
  expect(matches).toHaveLength(1)
  return matches[0]
}

function button(html: string, label: string): string {
  const matches = (html.match(/<button\b[\s\S]*?<\/button>/g) ?? []).filter((item) =>
    item.includes(label),
  )
  expect(matches).toHaveLength(1)
  return matches[0]
}

describe('unified plugin settings', () => {
  it('merges bundled setup and installed controls into one card per plugin', () => {
    const html = render({ settings: settings(), catalog })

    expect(articles(html)).toHaveLength(5)
    for (const name of ['独立浏览器', '电脑操作', 'Chrome', '外部插件', 'LLM Wiki']) {
      expect(article(html, name)).toContain(`启用插件 ${name}`)
    }
    expect(html).not.toContain('<strong>浏览器操作</strong>')
    expect(article(html, '电脑操作')).toContain('辅助功能 · 已授权')
    expect(article(html, '电脑操作')).toContain('屏幕录制 · 已授权')
    expect(article(html, 'Chrome')).toContain('Chrome 已连接')
    expect(article(html, 'Chrome')).toContain('卸载插件 Chrome')
  })

  it.each([
    { label: 'empty', catalog: [] },
    { label: 'not loaded', catalog: undefined },
    { label: 'failed', catalog: undefined, catalogError: new Error('内置目录读取失败') },
  ])('retains installed and external plugins when the catalog is $label', (state) => {
    const html = render({ settings: settings(), ...state })

    expect(articles(html)).toHaveLength(5)
    for (const item of installed) {
      const card = article(html, item.name)
      expect(button(card, `aria-label="启用插件 ${item.name}"`)).not.toContain('disabled=')
      expect(button(card, `aria-label="卸载插件 ${item.name}"`)).not.toContain('disabled=')
      expect(card).toContain(`<dd>${item.id}</dd>`)
    }
    expect(article(html, '外部插件')).toContain('更新插件 外部插件')
    if (state.catalogError) expect(html).toContain('内置目录读取失败')
  })

  it('keeps an unavailable installed plugin manageable while disabling bundled updates', () => {
    const unavailable: BundledPlugin = {
      ...catalog[1],
      available: false,
      reason: '此平台暂不支持电脑操作。',
    }
    const html = render({ settings: settings([installed[1]]), catalog: [unavailable] })
    const card = article(html, '电脑操作')

    expect(card).toContain(unavailable.reason)
    expect(button(card, '更新内置版本')).toContain('disabled=""')
    expect(button(card, 'aria-label="启用插件 电脑操作"')).not.toContain('disabled=')
    expect(button(card, 'aria-label="卸载插件 电脑操作"')).not.toContain('disabled=')
  })

  it('shows unknown installation state while settings are loading', () => {
    const html = render({ catalog, loading: true })

    for (const item of catalog) {
      const card = article(html, item.name)
      const header = card.match(/<div class="extension-identity">([\s\S]*?)<\/div>/)?.[1]
      expect(header).toContain('正在读取状态…')
      expect(header).not.toContain('未安装')
    }
    expect(button(article(html, 'Chrome'), '启用 Chrome')).toContain('disabled=""')
  })

  it('distinguishes a disabled Chrome plugin from its retained browser connection', () => {
    const html = render({
      settings: settings([plugin('chrome-use', 'Chrome', { enabled: false })]),
      catalog: [catalog[2]],
    })
    const card = article(html, 'Chrome')

    expect(card).toContain('插件已关闭 · Chrome 连接已保留')
    expect(button(card, 'aria-label="启用插件 Chrome"')).toContain('aria-checked="false"')
    expect(card).not.toContain('安装 Chrome 扩展')
  })

  it('offers an enable action when Chrome is connected but its plugin is not installed', () => {
    const html = render({ settings: settings([]), catalog: [catalog[2]] })
    const card = article(html, 'Chrome')

    expect(card).toContain('未安装')
    expect(card).toContain('Chrome 已连接')
    expect(button(card, '启用 Chrome')).not.toContain('disabled=')
    expect(card).not.toContain('卸载插件 Chrome')
  })

  it('preserves external plugin warnings and connection prerequisites', () => {
    const external = plugin('third-party', '外部插件', {
      warnings: ['插件来源需要核对。'],
      mcp_servers: [
        {
          id: 'external-server',
          name: '外部服务',
          enabled: true,
          transport: { type: 'stdio', command: 'node', args: [], env_vars: ['SERVICE_TOKEN'] },
        },
      ],
    })
    const html = render({ settings: settings([external]), catalog })
    const card = article(html, '外部插件')

    expect(card).toContain('插件来源需要核对。')
    expect(card).toContain('外部服务')
    expect(card).toContain('运行前需设置环境变量：SERVICE_TOKEN')
    expect(button(card, '测试连接')).not.toContain('disabled=')
  })
  it('links installed plugins to the unified permission manager', () => {
    const chrome = plugin('chrome-use', 'Chrome', {
      mcp_servers: [
        {
          id: 'plugin_chrome_use_chrome',
          name: 'Chrome',
          enabled: true,
          transport: { type: 'stdio', command: 'node', args: [], env_vars: [] },
        },
      ],
    })
    const configured = settings([chrome])
    configured.control_permissions = [
      {
        server_id: 'plugin_chrome_use_chrome',
        server_name: 'Chrome',
        capability: 'chrome',
      },
    ]
    const html = render({ settings: configured, catalog: [catalog[2]] })
    const card = article(html, 'Chrome')
    expect(card).toContain('已保存 1 项访问授权')
    expect(card).toContain('href="/settings"')
    expect(card).toContain('管理访问授权')
    expect(card).not.toContain('每个会话首次使用时询问')
    configured.control_permissions = []
    const cleared = article(render({ settings: configured, catalog: [catalog[2]] }), 'Chrome')
    expect(cleared).toContain('尚未保存长期授权')
    expect(cleared).not.toContain('取消始终允许')
  })
})
