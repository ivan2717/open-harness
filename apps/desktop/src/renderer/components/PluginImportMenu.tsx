import { Archive, ChevronDown, FolderOpen, Plus, RefreshCw } from 'lucide-react'
import type { PluginSourceKind } from '../../shared/extensions'
import { ComposerMenu } from './ComposerMenu'

export function PluginImportMenu({
  name,
  disabled,
  onChoose,
}: {
  name?: string
  disabled: boolean
  onChoose: (sourceKind: PluginSourceKind) => void
}) {
  return (
    <ComposerMenu
      label={name ? `更新插件 ${name}` : '安装插件'}
      className="extension-text-button plugin-import-button"
      align="end"
      disabled={disabled}
      trigger={
        <>
          {name ? <RefreshCw size={14} /> : <Plus size={15} />}
          {name ? '更新' : '安装插件'}
          <ChevronDown size={12} />
        </>
      }
    >
      <button
        type="button"
        role="menuitem"
        className="composer-menu-item"
        onClick={() => onChoose('directory')}
      >
        <FolderOpen size={15} />
        从文件夹{name ? '更新' : '安装'}
      </button>
      <button
        type="button"
        role="menuitem"
        className="composer-menu-item"
        onClick={() => onChoose('zip')}
      >
        <Archive size={15} />从 ZIP {name ? '更新' : '安装'}
      </button>
    </ComposerMenu>
  )
}
