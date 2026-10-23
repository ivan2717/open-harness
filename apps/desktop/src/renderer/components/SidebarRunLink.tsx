import { Link } from '@tanstack/react-router'
import type { RunSummary } from '../../shared/contracts'
import { dateTime } from '../lib/format'
import { RunActivity } from './RunActivity'

export function SidebarRunLink({ run, pathname }: { run: RunSummary; pathname: string }) {
  const active = pathname === `/runs/${encodeURIComponent(run.id)}`
  return (
    <Link
      to="/runs/$runId"
      params={{ runId: run.id }}
      className={`run-link${active ? ' is-active' : ''}`}
      aria-current={active ? 'page' : undefined}
      title={run.title || run.prompt}
    >
      <RunActivity active={run.status === 'running' || Boolean(run.compacting)} />
      <span className="min-w-0 flex-1">
        <span className="run-link-title">{run.title || run.prompt}</span>
        <time className="run-link-meta" dateTime={run.updated_at}>
          {dateTime(run.updated_at)}
        </time>
      </span>
    </Link>
  )
}
