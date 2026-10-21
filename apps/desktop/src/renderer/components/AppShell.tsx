import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, Outlet, useLocation, useNavigate } from '@tanstack/react-router'
import {
  Command,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Search,
  Settings2,
  WifiOff,
} from 'lucide-react'
import { configOptions, runsOptions } from '../lib/queries'
import { dateTime, statusLabels } from '../lib/format'

export function AppShell() {
  const location = useLocation()
  const navigate = useNavigate()
  const runs = useQuery(runsOptions)
  const config = useQuery(configOptions)
  const [search, setSearch] = useState('')
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const shownRuns = (runs.data ?? []).filter((run) =>
    `${run.title} ${run.prompt}`.toLocaleLowerCase().includes(search.toLocaleLowerCase()),
  )
  const isMac = window.harness?.platform === 'darwin'

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === 'n' &&
        !event.altKey &&
        !event.shiftKey
      ) {
        event.preventDefault()
        void navigate({ to: '/' })
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [navigate])

  return (
    <div className="desktop-shell">
      <div className={`titlebar ${isMac ? 'titlebar-mac' : ''}`}>
        <button
          className="icon-button no-drag"
          aria-label={sidebarOpen ? '收起侧边栏' : '展开侧边栏'}
          onClick={() => setSidebarOpen(!sidebarOpen)}
        >
          {sidebarOpen ? <PanelLeftClose size={16} /> : <PanelLeftOpen size={16} />}
        </button>
        <span className="titlebar-name">Open Harness</span>
        {config.isError && (
          <span className="titlebar-local" role="status">
            <span className="connection-dot is-offline" />
            连接中断
          </span>
        )}
      </div>
      <div className="desktop-body">
        {sidebarOpen && (
          <aside className="sidebar" aria-label="会话导航">
            <Link to="/" className={`new-run ${location.pathname === '/' ? 'is-active' : ''}`}>
              <Plus size={16} />
              <span>新建会话</span>
              <span className="keycap">{isMac ? <Command size={10} /> : 'Ctrl'} N</span>
            </Link>
            <label className="search-field">
              <Search size={14} />
              <input
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder="搜索会话"
                aria-label="搜索会话"
              />
            </label>
            <div className="sidebar-section-heading">
              <span>最近会话</span>
              <span>{runs.data?.length ?? 0}</span>
            </div>
            <nav className="run-list">
              {runs.isPending && <div className="sidebar-empty">正在读取会话…</div>}
              {runs.isError && (
                <div className="sidebar-empty">
                  <WifiOff size={18} />
                  <span>会话加载失败</span>
                  <button onClick={() => void runs.refetch()}>重新加载</button>
                </div>
              )}
              {!runs.isPending && !runs.isError && !shownRuns.length && (
                <div className="sidebar-empty">
                  <span>{search ? '没有匹配的会话' : '暂无会话'}</span>
                </div>
              )}
              {shownRuns.map((run) => (
                <Link
                  key={run.id}
                  to="/runs/$runId"
                  params={{ runId: run.id }}
                  className={`run-link ${location.pathname === `/runs/${encodeURIComponent(run.id)}` ? 'is-active' : ''}`}
                >
                  <span className={`status-dot status-${run.status}`} />
                  <span className="min-w-0 flex-1">
                    <span className="run-link-title">{run.title || run.prompt}</span>
                    <span className="run-link-meta">
                      {dateTime(run.updated_at)}
                      <span>·</span>
                      {statusLabels[run.status]}
                    </span>
                  </span>
                </Link>
              ))}
            </nav>
            <div className="sidebar-bottom">
              <Link
                to="/settings"
                className={`settings-link ${location.pathname === '/settings' ? 'is-active' : ''}`}
              >
                <Settings2 size={16} />
                工作区设置
                <span className="ml-auto text-[11px] text-muted">
                  v{config.data?.version ?? '—'}
                </span>
              </Link>
            </div>
          </aside>
        )}
        <main className="workspace">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
