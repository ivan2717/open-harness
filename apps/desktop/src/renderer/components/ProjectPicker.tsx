import { Check, ChevronDown, FolderOpen, Laptop, ShieldCheck, X } from 'lucide-react'
import type { ProjectSelection } from '../state/ui-store'
import { ComposerMenu } from './ComposerMenu'

export function ProjectPicker({
  project,
  onChoose,
  onChange,
  disabled,
}: {
  project?: ProjectSelection | null
  onChoose?: () => void
  onChange?: (project: ProjectSelection | null) => void
  disabled?: boolean
}) {
  const name = project?.path.split(/[\\/]/).filter(Boolean).at(-1)
  return (
    <div className="composer-context-bar">
      <div className="composer-project-controls">
        {onChoose && project ? (
          <ComposerMenu
            label="项目"
            className="composer-context-project"
            title={project.path}
            disabled={disabled}
            trigger={
              <>
                <FolderOpen size={16} />
                <span className="truncate">{name}</span>
                <ChevronDown size={12} />
              </>
            }
          >
            <button role="menuitem" className="composer-menu-item" onClick={onChoose}>
              <FolderOpen size={15} />
              更换项目
            </button>
            {onChange && (
              <button role="menuitem" className="composer-menu-item" onClick={() => onChange(null)}>
                <X size={15} />
                移除项目
              </button>
            )}
          </ComposerMenu>
        ) : onChoose ? (
          <button
            className="composer-context-project"
            aria-label="选择项目"
            title="选择 Agent 可以访问的本地项目"
            disabled={disabled}
            onClick={onChoose}
          >
            <FolderOpen size={16} />
            <span>选择项目</span>
          </button>
        ) : (
          <span
            className="composer-context-project"
            title={project?.path ?? '此会话未关联本地项目'}
          >
            <FolderOpen size={16} />
            <span className="truncate">{name ?? '未关联项目'}</span>
          </span>
        )}
        {project && <ProjectPermission project={project} onChange={onChange} disabled={disabled} />}
      </div>
      <span className="composer-environment" title="任务在当前计算机执行">
        <Laptop size={16} />
        此计算机
      </span>
    </div>
  )
}

function ProjectPermission({
  project,
  onChange,
  disabled,
}: {
  project: ProjectSelection
  onChange?: (project: ProjectSelection | null) => void
  disabled?: boolean
}) {
  const label = project.permission === 'read_only' ? '只读' : '允许修改'
  if (!onChange)
    return (
      <span
        className="composer-permission-label"
        title="此会话的权限已固定。允许修改时，命令仍需逐次批准；更改权限请新建会话。"
      >
        <ShieldCheck size={15} />
        {label}
      </span>
    )
  return (
    <ComposerMenu
      label="项目权限"
      disabled={disabled}
      trigger={
        <>
          <ShieldCheck size={15} />
          {label}
          <ChevronDown size={12} />
        </>
      }
    >
      <div className="composer-menu-heading">项目权限</div>
      {(['read_only', 'workspace_write'] as const).map((permission) => (
        <button
          key={permission}
          role="menuitemradio"
          aria-checked={project.permission === permission}
          className="composer-menu-item"
          onClick={() => onChange({ ...project, permission })}
        >
          <span>
            <strong>{permission === 'read_only' ? '只读' : '允许修改'}</strong>
            <small>
              {permission === 'read_only'
                ? '读取和搜索文件，不修改项目。'
                : '可修改项目文件；命令需单独批准。'}
            </small>
          </span>
          {project.permission === permission && <Check size={15} className="menu-check" />}
        </button>
      ))}
    </ComposerMenu>
  )
}
