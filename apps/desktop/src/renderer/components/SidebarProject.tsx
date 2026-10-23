import { useEffect, useId, useState } from 'react'
import { ChevronDown, FolderOpen, SquarePen } from 'lucide-react'
import type { SidebarProjectGroup } from '../lib/sidebar-projects'
import { SidebarRunLink } from './SidebarRunLink'

export function SidebarProject({
  project,
  pathname,
  selected,
  search,
  busy,
  onCreate,
}: {
  project: SidebarProjectGroup
  pathname: string
  selected: boolean
  search: string
  busy: boolean
  onCreate: () => void
}) {
  const [expanded, setExpanded] = useState(true)
  const id = useId()
  const activeRun = project.runs.find(
    (run) => pathname === `/runs/${encodeURIComponent(run.id)}`,
  )?.id
  useEffect(() => {
    if (activeRun || selected || search) setExpanded(true)
  }, [activeRun, selected, search])
  const open = expanded
  return (
    <div
      className={`sidebar-project${activeRun || selected ? ' is-current' : ''}`}
      data-project-path={project.path}
    >
      <div className="sidebar-project-heading">
        <button
          className="sidebar-project-toggle"
          type="button"
          aria-label={`${open ? '收起' : '展开'}项目 ${project.name}`}
          aria-expanded={open}
          aria-controls={id}
          title={project.path}
          onClick={() => setExpanded(!open)}
        >
          <FolderOpen size={16} aria-hidden="true" />
          <span className="sidebar-project-name">{project.name}</span>
          <span className="sidebar-project-count">{project.total}</span>
          <ChevronDown size={12} className={open ? '' : 'is-collapsed'} aria-hidden="true" />
        </button>
        <button
          className="sidebar-project-create"
          type="button"
          aria-label={`在 ${project.name} 中新建会话`}
          title={`在此项目中新建会话 · ${project.path}`}
          disabled={busy}
          onClick={() => {
            setExpanded(true)
            onCreate()
          }}
        >
          <SquarePen size={14} aria-hidden="true" />
        </button>
      </div>
      <nav
        id={id}
        className="sidebar-project-conversations"
        aria-label={`${project.path} 的会话`}
        hidden={!open}
      >
        {project.runs.map((run) => (
          <SidebarRunLink key={run.id} run={run} pathname={pathname} />
        ))}
        {project.total === 0 && <p className="sidebar-project-empty">暂无会话</p>}
      </nav>
    </div>
  )
}
