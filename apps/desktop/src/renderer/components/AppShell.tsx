import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Outlet, useLocation, useMatch, useNavigate } from '@tanstack/react-router'
import { PanelLeftClose, PanelLeftOpen } from 'lucide-react'
import { configOptions } from '../lib/queries'
import { ChatSidebar } from './ChatSidebar'
import { NavigationRail } from './NavigationRail'
import { useUiStore } from '../state/ui-store'

export function AppShell() {
  const location = useLocation()
  const navigate = useNavigate()
  const runMatch = useMatch({ from: '/runs/$runId', shouldThrow: false })
  const runId = runMatch?.params.runId
  const config = useQuery(configOptions)
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [lastRunId, setLastRunId] = useState<string | null>(null)
  const isChat = location.pathname === '/' || Boolean(runMatch)
  const isMac = window.harness?.platform === 'darwin'

  useEffect(() => {
    if (runId) setLastRunId(runId)
    else if (location.pathname === '/') setLastRunId(null)
  }, [runId, location.pathname])

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === 'n' &&
        !event.altKey &&
        !event.shiftKey
      ) {
        event.preventDefault()
        useUiStore.getState().setProject(null)
        void navigate({ to: '/' })
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [navigate])

  return (
    <div className="desktop-shell">
      <div className={`titlebar ${isMac ? 'titlebar-mac' : ''}`}>
        {isChat && (
          <button
            className="icon-button no-drag"
            aria-label={sidebarOpen ? '收起侧边栏' : '展开侧边栏'}
            aria-expanded={sidebarOpen}
            onClick={() => setSidebarOpen(!sidebarOpen)}
          >
            {sidebarOpen ? <PanelLeftClose size={16} /> : <PanelLeftOpen size={16} />}
          </button>
        )}
        <span className="titlebar-name">Open Harness</span>
        {config.isError && (
          <span className="titlebar-local" role="status">
            <span className="connection-dot is-offline" />
            连接中断
          </span>
        )}
      </div>
      <div className="desktop-body">
        <NavigationRail pathname={location.pathname} runId={isChat ? (runId ?? null) : lastRunId} />
        <ChatSidebar visible={isChat && sidebarOpen} pathname={location.pathname} />
        <main className="workspace">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
