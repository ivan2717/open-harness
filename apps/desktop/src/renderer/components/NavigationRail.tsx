import { type ReactNode } from 'react'
import { Link } from '@tanstack/react-router'
import { Cpu, House, Puzzle, Settings2 } from 'lucide-react'

function RailItem({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="navigation-rail-item">
      {children}
      <span className="navigation-rail-tooltip" role="tooltip">
        {label}
      </span>
    </div>
  )
}

export function NavigationRail({ pathname, runId }: { pathname: string; runId: string | null }) {
  const isChat = pathname === '/' || pathname.startsWith('/runs/')
  const linkClass = (active: boolean) => `navigation-rail-link${active ? ' is-active' : ''}`
  return (
    <nav className="navigation-rail" aria-label="主导航">
      <div className="navigation-rail-primary">
        <RailItem label="会话">
          {runId ? (
            <Link
              to="/runs/$runId"
              params={{ runId }}
              className={linkClass(isChat)}
              aria-label="会话"
              aria-current={isChat ? 'page' : undefined}
            >
              <House size={21} />
            </Link>
          ) : (
            <Link
              to="/"
              className={linkClass(isChat)}
              aria-label="会话"
              aria-current={isChat ? 'page' : undefined}
            >
              <House size={21} />
            </Link>
          )}
        </RailItem>
        <div className="navigation-rail-divider" />
        <RailItem label="插件与技能">
          <Link
            to="/extensions"
            search={{ tab: 'plugins' }}
            className={linkClass(pathname === '/extensions')}
            aria-label="插件"
            aria-current={pathname === '/extensions' ? 'page' : undefined}
          >
            <Puzzle size={21} />
          </Link>
        </RailItem>
        <RailItem label="模型">
          <Link
            to="/models"
            className={linkClass(pathname === '/models')}
            aria-label="模型"
            aria-current={pathname === '/models' ? 'page' : undefined}
          >
            <Cpu size={21} />
          </Link>
        </RailItem>
      </div>
      <RailItem label="设置">
        <Link
          to="/settings"
          className={linkClass(pathname === '/settings')}
          aria-label="设置"
          aria-current={pathname === '/settings' ? 'page' : undefined}
        >
          <Settings2 size={21} />
        </Link>
      </RailItem>
    </nav>
  )
}
