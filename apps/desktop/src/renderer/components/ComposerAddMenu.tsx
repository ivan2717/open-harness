import { BookOpen, LoaderCircle, Paperclip, Plus } from 'lucide-react'
import { ComposerMenu } from './ComposerMenu'

export function ComposerAddMenu({
  disabled,
  pending,
  onFiles,
  onSkills,
}: {
  disabled?: boolean
  pending?: boolean
  onFiles: () => void
  onSkills: () => void
}) {
  const pasteKey = window.harness.platform === 'darwin' ? '⌘V' : 'Ctrl+V'
  return (
    <ComposerMenu
      label="添加内容"
      className="composer-attach icon-button"
      disabled={disabled}
      tooltip={
        <>
          <span>添加文件或使用技能</span>
          <small>文件也可拖入或按 {pasteKey} 粘贴</small>
        </>
      }
      trigger={
        pending ? (
          <LoaderCircle size={18} className="animate-spin" />
        ) : (
          <Plus size={21} strokeWidth={1.8} />
        )
      }
    >
      <button type="button" role="menuitem" className="composer-menu-item" onClick={onFiles}>
        <Paperclip size={15} />
        添加文件
      </button>
      <button type="button" role="menuitem" className="composer-menu-item" onClick={onSkills}>
        <BookOpen size={15} />
        使用技能
      </button>
    </ComposerMenu>
  )
}
