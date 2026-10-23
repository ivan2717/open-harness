import { useMemo, useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ChevronDown, FolderOpen, RefreshCw } from 'lucide-react'
import { extensionSettingsOptions, useSaveExtensions, withSkillEnabled } from '../lib/extensions'
import { errorMessage } from '../lib/format'
import { runsOptions } from '../lib/queries'
import { projectName } from '../lib/sidebar-projects'
import { useUiStore } from '../state/ui-store'
import { InlineError } from './Feedback'
import { SkillDiagnostics, SkillList } from './SkillList'

export function SkillsSettings() {
  const settings = useQuery(extensionSettingsOptions)
  const runs = useQuery(runsOptions)
  const projectPaths = useUiStore((state) => state.projectPaths)
  const [workspacePath, setWorkspacePath] = useState('')
  const paths = useMemo(
    () =>
      [...new Set([...projectPaths, ...(runs.data ?? []).map((run) => run.workspace_path)])]
        .filter((path): path is string => typeof path === 'string' && Boolean(path))
        .sort((left, right) => left.localeCompare(right)),
    [projectPaths, runs.data],
  )
  const selecting = useMutation({
    mutationFn: (path: string) => (path ? window.harness.openWorkspace(path) : Promise.resolve('')),
    onSuccess: setWorkspacePath,
  })
  const projectCatalog = useQuery({
    queryKey: ['skill-catalog', workspacePath],
    queryFn: () => window.harness.skillCatalog(workspacePath),
    enabled: Boolean(workspacePath),
    retry: 1,
  })
  const catalog = workspacePath ? projectCatalog : settings
  const skills = catalog.data?.skills.filter((skill) => !skill.plugin_id) ?? []
  const save = useSaveExtensions()
  const openDirectory = useMutation({ mutationFn: () => window.harness.openSkillsDirectory() })
  const busy = save.isPending || selecting.isPending

  return (
    <section className="settings-section extension-settings" aria-label="技能管理">
      <div className="extension-section-heading">
        <div>
          <h2>技能</h2>
          <p>管理独立安装的技能。插件附带的技能在「插件」中管理。</p>
        </div>
        <div className="extension-actions">
          {!workspacePath && (
            <button
              type="button"
              className="extension-text-button"
              disabled={openDirectory.isPending || !settings.data}
              onClick={() => openDirectory.mutate()}
            >
              <FolderOpen size={15} />
              打开文件夹
            </button>
          )}
          <button
            type="button"
            className="extension-icon-button"
            aria-label="刷新技能"
            title="刷新技能"
            disabled={catalog.isFetching || busy}
            onClick={() => void catalog.refetch()}
          >
            <RefreshCw size={15} className={catalog.isFetching ? 'animate-spin' : undefined} />
          </button>
        </div>
      </div>
      <div className="skill-scope-field">
        <label htmlFor="skill-scope">技能范围</label>
        <select
          id="skill-scope"
          aria-label="技能范围"
          value={workspacePath}
          disabled={busy}
          onChange={(event) => {
            save.reset()
            selecting.mutate(event.target.value)
          }}
        >
          <option value="">个人技能</option>
          {paths.map((path) => (
            <option key={path} value={path}>
              {projectName(path)} · {path}
            </option>
          ))}
        </select>
      </div>
      <p className="extension-hint">
        {workspacePath && '显示此项目可用的技能；同名技能优先使用项目版本。'}
        开关会影响所有会话中的同名技能。
      </p>
      {selecting.isPending && (
        <p className="extension-hint" role="status">
          正在打开项目…
        </p>
      )}
      {selecting.isError && <InlineError message={errorMessage(selecting.error)} />}
      {catalog.isPending && <p className="extension-hint">正在读取技能…</p>}
      {catalog.isError && (
        <InlineError message={errorMessage(catalog.error)} retry={() => void catalog.refetch()} />
      )}
      {catalog.data && (
        <SkillList
          skills={skills}
          busy={busy}
          onToggle={(id, enabled) =>
            save.mutate((current) => withSkillEnabled(current, id, enabled))
          }
        />
      )}
      {catalog.data && skills.length === 0 && (
        <p className="extension-empty">
          {workspacePath
            ? '此项目暂无独立技能。将技能文件夹放入项目的 .agents/skills，再刷新即可。'
            : '尚未安装个人技能。将包含 SKILL.md 的技能文件夹放入此目录，再刷新即可。'}
        </p>
      )}
      <details className="extension-install-help">
        <summary>
          <ChevronDown size={13} />
          安装与使用
        </summary>
        {!workspacePath && settings.data && <code>{settings.data.skills_directory}</code>}
        {workspacePath && <code>{workspacePath}</code>}
        <p>
          每个技能使用独立文件夹，SKILL.md 开头的 YAML 填写 name 和
          description，其后编写操作说明；可附带 references、scripts 等资源。
        </p>
        <p>
          项目中的 .agents/skills 和 .openharness/skills 会自动发现，在上方切换技能范围即可管理。
        </p>
        <p>
          在输入框的「＋」中选择技能，或输入 $技能名称，可指定这条消息使用的技能；Agent
          也可按描述选择。技能只提供工作流程，读取脚本不会自动执行。
        </p>
      </details>
      {catalog.data && <SkillDiagnostics diagnostics={catalog.data.diagnostics} />}
      {save.isError && <InlineError message={errorMessage(save.error)} />}
      {openDirectory.isError && <InlineError message={errorMessage(openDirectory.error)} />}
    </section>
  )
}
