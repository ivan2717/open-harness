import { ChevronDown } from 'lucide-react'
import type { SkillInfo } from '../../shared/extensions'

const scopeNames: Record<SkillInfo['scope'], string> = {
  project: '项目',
  user: '个人',
  additional: '附加目录',
}

export function SkillList({
  skills,
  busy,
  onToggle,
}: {
  skills: SkillInfo[]
  busy: boolean
  onToggle: (id: string, enabled: boolean) => void
}) {
  return (
    <div className="extension-list">
      {skills.map((skill) => (
        <div className="extension-row" key={skill.id}>
          <details className="skill-description">
            <summary>
              <ChevronDown size={13} />
              <strong>{skill.name}</strong>
              <span className="extension-scope">
                {skill.plugin_id ? `插件 · ${skill.plugin_id}` : scopeNames[skill.scope]}
              </span>
            </summary>
            <p>{skill.description}</p>
            <code>{skill.path}</code>
            <p className="extension-hint">在消息中输入 ${skill.id} 即可使用。</p>
          </details>
          <button
            type="button"
            role="switch"
            aria-checked={skill.enabled}
            aria-label={`启用技能 ${skill.plugin_id ? skill.id : skill.name}`}
            className={`toggle-switch ${skill.enabled ? 'is-on' : ''}`}
            disabled={busy}
            onClick={() => onToggle(skill.id, !skill.enabled)}
          >
            <span />
          </button>
        </div>
      ))}
    </div>
  )
}

export function SkillDiagnostics({
  diagnostics,
}: {
  diagnostics: { path: string; message: string }[]
}) {
  if (!diagnostics.length) return null
  return (
    <details className="extension-diagnostics">
      <summary>
        <ChevronDown size={13} />
        {diagnostics.length} 条技能发现提示
      </summary>
      <ul>
        {diagnostics.map((item, index) => (
          <li key={`${item.path}-${index}`}>
            <p>{item.message}</p>
            <code>{item.path}</code>
          </li>
        ))}
      </ul>
    </details>
  )
}
