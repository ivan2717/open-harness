import { useMemo, useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useRouter } from '@tanstack/react-router'
import { Command, LoaderCircle, Plus, Search, WifiOff } from 'lucide-react'
import { runsOptions } from '../lib/queries'
import { errorMessage } from '../lib/format'
import { sidebarConversations } from '../lib/sidebar-projects'
import { useUiStore } from '../state/ui-store'
import { SidebarProject } from './SidebarProject'
import { SidebarRunLink } from './SidebarRunLink'
import '../sidebar-projects.css'

type ProjectAction = { type: 'add' } | { type: 'open'; path: string }

export function ChatSidebar({ visible, pathname }: { visible: boolean; pathname: string }) {
  const runs = useQuery(runsOptions)
  const [search, setSearch] = useState('')
  const project = useUiStore((state) => state.project)
  const projectPaths = useUiStore((state) => state.projectPaths)
  const setProject = useUiStore((state) => state.setProject)
  const navigate = useNavigate()
  const router = useRouter()
  const isMac = window.harness?.platform === 'darwin'
  const currentRun = runs.data?.find((run) => pathname === `/runs/${encodeURIComponent(run.id)}`)
  const conversationPath = currentRun?.delegation
    ? `/runs/${encodeURIComponent(currentRun.delegation.root_run_id)}`
    : pathname
  const groups = useMemo(
    () =>
      sidebarConversations(
        runs.data ?? [],
        [...projectPaths, ...(project ? [project.path] : [])],
        search,
      ),
    [runs.data, projectPaths, project, search],
  )
  const selecting = useMutation({
    mutationFn: (action: ProjectAction) =>
      action.type === 'add'
        ? window.harness.chooseWorkspace()
        : window.harness.openWorkspace(action.path),
    onMutate: () => ({
      location: router.state.location,
      project: useUiStore.getState().project,
    }),
    onSuccess: async (path, _action, context) => {
      if (!path) return
      // Opening a saved project can involve disk/runtime I/O. Respect navigation
      // or a different draft selection made while that request was pending.
      if (
        router.state.location !== context?.location ||
        useUiStore.getState().project !== context.project
      )
        return
      setProject({ path, permission: 'workspace_write' })
      useUiStore.getState().resetFullAccessDraft()
      setSearch('')
      await navigate({ to: '/' })
      requestAnimationFrame(() =>
        document.querySelector<HTMLTextAreaElement>('.welcome-composer textarea')?.focus(),
      )
    },
  })
  const noMatches = !groups.independent.length && !groups.projects.length
  return (
    <aside className="sidebar" aria-label="会话导航" hidden={!visible}>
      <Link
        to="/"
        className={`new-run ${pathname === '/' && !project ? 'is-active' : ''}`}
        aria-disabled={selecting.isPending || undefined}
        onClick={(event) => {
          if (selecting.isPending) event.preventDefault()
          else {
            setProject(null)
            useUiStore.getState().resetFullAccessDraft()
          }
        }}
      >
        <Plus size={16} />
        <span>新建会话</span>
        <span className="keycap">{isMac ? <Command size={10} /> : 'Ctrl'} N</span>
      </Link>
      <label className="search-field">
        <Search size={14} />
        <input
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder="搜索会话和项目"
          aria-label="搜索会话"
        />
      </label>
      <div className="sidebar-scroll">
        {runs.isPending && <div className="sidebar-empty">正在读取会话…</div>}
        {runs.isError && (
          <div className="sidebar-empty">
            <WifiOff size={18} />
            <span>会话加载失败</span>
            <button onClick={() => void runs.refetch()}>重新加载</button>
          </div>
        )}
        <section className="sidebar-project-section" aria-label="项目">
          <div className="sidebar-section-heading">
            <span>项目</span>
            <button
              className="sidebar-add-project"
              type="button"
              aria-label="添加项目"
              title="添加项目"
              disabled={selecting.isPending}
              onClick={() => selecting.mutate({ type: 'add' })}
            >
              {selecting.isPending && selecting.variables.type === 'add' ? (
                <LoaderCircle size={14} className="animate-spin" aria-hidden="true" />
              ) : (
                <Plus size={14} aria-hidden="true" />
              )}
            </button>
          </div>
          {selecting.isError && (
            <p className="sidebar-project-error" role="alert">
              {errorMessage(selecting.error)}
            </p>
          )}
          {groups.projects.map((group) => (
            <SidebarProject
              key={group.path}
              project={group}
              pathname={conversationPath}
              selected={pathname === '/' && project?.path === group.path}
              search={search.trim()}
              busy={selecting.isPending}
              onCreate={() => selecting.mutate({ type: 'open', path: group.path })}
            />
          ))}
          {!search.trim() && groups.projects.length === 0 && (
            <p className="sidebar-project-hint">添加文件夹，按项目整理会话。</p>
          )}
        </section>
        {groups.independent.length > 0 && (
          <section className="sidebar-conversation-section">
            <div className="sidebar-section-heading">
              <span>会话</span>
              <span>{search.trim() ? groups.independent.length : groups.independentTotal}</span>
            </div>
            <nav className="run-list" aria-label="会话">
              {groups.independent.map((run) => (
                <SidebarRunLink key={run.id} run={run} pathname={conversationPath} />
              ))}
            </nav>
          </section>
        )}
        {!runs.isPending && !runs.isError && noMatches && (
          <div className="sidebar-empty">
            <span>{search.trim() ? '没有匹配的会话或项目' : '暂无会话'}</span>
          </div>
        )}
      </div>
    </aside>
  )
}
