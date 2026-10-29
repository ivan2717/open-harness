import { ChevronDown, FolderOpen, Laptop, X } from 'lucide-react'
import type { ProjectSelection } from '../state/ui-store'
import { ComposerMenu } from './ComposerMenu'

export function ProjectPicker({
  project,
  executionDirectory,
  onRevealWorkspace,
  onChoose,
  onChange,
  disabled,
}: {
  project?: ProjectSelection | null
  executionDirectory?: string | null
  onRevealWorkspace?: () => void
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
        ) : executionDirectory && !project ? (
          <button
            type="button"
            className="composer-context-project"
            aria-label="打开会话文件夹"
            title={`此会话的脚本和生成文件保存在这里：${executionDirectory}`}
            onClick={onRevealWorkspace}
            disabled={!onRevealWorkspace}
          >
            <FolderOpen size={16} />
            <span>会话文件</span>
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
      </div>
      <span className="composer-environment" title="任务在当前计算机执行">
        <Laptop size={16} />
        此计算机
      </span>
    </div>
  )
}
