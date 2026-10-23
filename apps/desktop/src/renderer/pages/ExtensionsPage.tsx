import { Link, useSearch } from '@tanstack/react-router'
import { Puzzle } from 'lucide-react'
import { ExtensionsSettings } from '../components/ExtensionsSettings'

const sections = [
  { id: 'plugins', label: '插件' },
  { id: 'skills', label: '技能' },
  { id: 'mcp', label: 'MCP 服务' },
] as const

export function ExtensionsPage() {
  const search = useSearch({ strict: false })
  const section = search.tab === 'skills' || search.tab === 'mcp' ? search.tab : 'plugins'
  return (
    <div className="settings-page management-page">
      <header className="workspace-header">
        <div className="workspace-heading">
          <Puzzle size={16} />
          <h1 className="management-page-title">扩展</h1>
        </div>
      </header>
      <div className="settings-scroll">
        <div className="settings-content management-content">
          <nav className="management-tabs" aria-label="扩展分类">
            {sections.map((item) => (
              <Link
                key={item.id}
                to="/extensions"
                search={{ tab: item.id }}
                aria-current={section === item.id ? 'page' : undefined}
                className={section === item.id ? 'is-selected' : undefined}
              >
                {item.label}
              </Link>
            ))}
          </nav>
          <div className="management-tab-content">
            <ExtensionsSettings key={section} section={section} />
          </div>
        </div>
      </div>
    </div>
  )
}
