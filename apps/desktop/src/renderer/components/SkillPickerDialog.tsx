import { useEffect, useId, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { BookOpen, LoaderCircle, Search, X } from 'lucide-react'
import type { SkillInfo } from '../../shared/extensions'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'
import '../skill-picker.css'

export function SkillPickerDialog({
  runId,
  workspacePath,
  onChoose,
  onClose,
}: {
  runId?: string
  workspacePath?: string
  onChoose: (skill: SkillInfo) => void
  onClose: () => void
}) {
  const id = useId()
  const dialog = useRef<HTMLDialogElement>(null)
  const searchField = useRef<HTMLInputElement>(null)
  const [search, setSearch] = useState('')
  const query = useQuery({
    queryKey: runId ? ['skill-picker', runId] : ['skill-catalog', workspacePath ?? null],
    queryFn: () =>
      runId ? window.harness.refreshRunSkills(runId) : window.harness.skillCatalog(workspacePath),
    staleTime: 0,
    refetchOnMount: 'always',
    retry: 1,
  })
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    searchField.current?.focus()
    return () => {
      element?.close()
      if (previous?.isConnected) previous.focus()
    }
  }, [])
  const available = query.data?.skills.filter((skill) => skill.enabled) ?? []
  const term = search.trim().toLocaleLowerCase()
  const matches = available.filter((skill) =>
    `${skill.name} ${skill.id} ${skill.description}`.toLocaleLowerCase().includes(term),
  )
  return createPortal(
    <dialog
      ref={dialog}
      className="skill-picker"
      aria-labelledby={id}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return
        const bounds = event.currentTarget.getBoundingClientRect()
        if (
          event.clientX < bounds.left ||
          event.clientX > bounds.right ||
          event.clientY < bounds.top ||
          event.clientY > bounds.bottom
        )
          onClose()
      }}
    >
      <header>
        <h2 id={id}>选择技能</h2>
        <button type="button" className="icon-button" aria-label="关闭技能选择" onClick={onClose}>
          <X size={17} />
        </button>
      </header>
      <label className="skill-picker-search">
        <Search size={15} />
        <input
          ref={searchField}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          aria-label="搜索技能"
          placeholder="搜索技能…"
        />
      </label>
      <div className="skill-picker-results" aria-busy={query.isFetching}>
        {query.isFetching ? (
          <p className="skill-picker-hint">
            <LoaderCircle size={14} className="animate-spin" />
            正在读取技能…
          </p>
        ) : query.isError ? (
          <InlineError message={errorMessage(query.error)} retry={() => void query.refetch()} />
        ) : matches.length ? (
          <ul>
            {matches.map((skill) => (
              <li key={skill.id}>
                <button
                  type="button"
                  aria-label={`使用技能 ${skill.id}`}
                  onClick={() => onChoose(skill)}
                >
                  <BookOpen size={16} />
                  <span>
                    <strong>{skill.name}</strong>
                    <small>
                      {skill.plugin_id
                        ? `插件 · ${skill.plugin_id}`
                        : skill.scope === 'project'
                          ? '项目技能'
                          : '个人技能'}{' '}
                      · ${skill.id}
                    </small>
                    <p>{skill.description}</p>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="skill-picker-hint">
            {available.length ? '没有匹配的技能。' : '暂无可用技能，可在「插件」中安装或启用。'}
          </p>
        )}
      </div>
      <footer>
        <span>选择后加入当前消息，发送后使用。</span>
        <Link to="/extensions" search={{ tab: 'skills' }} onClick={onClose}>
          管理技能
        </Link>
      </footer>
    </dialog>,
    document.body,
  )
}
