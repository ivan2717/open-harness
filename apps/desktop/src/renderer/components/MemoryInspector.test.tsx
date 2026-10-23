import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import type { RunMemory } from '../../shared/memory'
import { MemorySnapshot } from './MemoryInspector'
import { BuiltinPluginCard } from './PluginSettings'

const saved: RunMemory = {
  enabled: true,
  status: 'ready',
  revision: 2,
  through_sequence: 15,
  overview: '目标：**离线整理资料**。',
  pages: [
    {
      id: 'decisions',
      title: '已作决定',
      content: '- 保留原始资料\n- 使用本地文件',
      source_sequences: [3, 10],
      updated_through: 15,
    },
  ],
  error: null,
}

const render = (memory: RunMemory) =>
  renderToStaticMarkup(<MemorySnapshot memory={memory} onSource={() => {}} />)

describe('run memory presentation', () => {
  it('shows readable Markdown, topic pages, revision and event provenance', () => {
    const html = render(saved)
    expect(html).toContain('<strong>离线整理资料</strong>')
    expect(html).toContain('<li>保留原始资料</li>')
    expect(html).toContain('已作决定')
    expect(html).toContain('版本 2 · 已整理至事件 #15')
    expect(html).toContain('在时间线中查看事件 #3')
    expect(html).toContain('在时间线中查看事件 #10')
  })

  it('keeps saved pages visible when disabled or a later update fails', () => {
    const disabled = render({ ...saved, enabled: false, status: 'disabled', pending: true })
    expect(disabled).toContain('已关闭')
    expect(disabled).toContain('离线整理资料')
    expect(disabled).toContain('已作决定')
    expect(disabled).toContain('上下文压缩和原始会话回查不受影响')
    expect(disabled).not.toContain('还有内容待整理')
    const failed = render({ ...saved, status: 'error', error: '整理超时' })
    expect(failed).toContain('role="alert"')
    expect(failed).toContain('整理超时')
    expect(failed).toContain('已保存的记忆保持不变')
    expect(failed).toContain('已作决定')
  })

  it('distinguishes an empty wiki without displaying a made-up revision', () => {
    const html = render({
      ...saved,
      status: 'empty',
      revision: 0,
      overview: '',
      pages: [],
      through_sequence: null,
    })
    expect(html).toContain('尚无记忆')
    expect(html).not.toContain('版本 0')
  })

  it('never loads images, activates links or executes HTML in generated memory', () => {
    const html = render({
      ...saved,
      overview:
        '<script>attack()</script>\n\n![image](https://example.com/track.png)\n\n[open](file:///private/data)\n\n[web](https://example.com)',
    })
    expect(html).not.toContain('<script')
    expect(html).not.toContain('<img')
    expect(html).not.toContain('src=')
    expect(html).not.toContain('href=')
  })

  it('offers only an enable switch for the built-in plugin', () => {
    const html = renderToStaticMarkup(
      <BuiltinPluginCard
        plugin={{
          id: 'builtin.llm-wiki',
          name: 'LLM Wiki',
          description: '会话记忆',
          enabled: true,
        }}
        busy={false}
        onToggle={() => {}}
      />,
    )
    expect(html).toContain('role="switch"')
    expect(html).toContain('aria-checked="true"')
    expect(html).toContain('已保存记录保留')
    expect(html).toContain('上下文压缩和原始会话回查不受影响')
    expect(html).not.toContain('卸载')
    expect(html).not.toContain('更新插件')
  })

  it('shows partial coverage and distinguishes plans from evidenced results', () => {
    const html = render({
      ...saved,
      pending: true,
      source_cursor: { sequence: 15, offset: 4000 },
      tasks: [
        {
          id: 'test',
          kind: 'work',
          status: 'planned',
          text: '运行测试',
          evidence: [{ sequence: 3, quote: '准备运行测试' }],
        },
        {
          id: 'test-failed',
          kind: 'work',
          status: 'failed',
          text: '测试未通过',
          evidence: [{ sequence: 10, quote: 'exit_code: 1' }],
        },
      ],
    })
    expect(html).toContain('事件 #15 分段整理中')
    expect(html).not.toContain('已整理至事件 #15')
    expect(html).toContain('还有内容待整理')
    expect(html).toContain('计划中')
    expect(html).toContain('失败')
    expect(html).toContain('准备运行测试')
    expect(html).toContain('来源 #10')
    expect(html).not.toContain('有完成结果')
  })

  it('shows the ingestion cursor rather than the verified prefix during legacy backfill', () => {
    const html = render({ ...saved, pending: true, source_cursor: { sequence: 4, offset: 0 } })
    expect(html).toContain('已整理至事件 #3')
    expect(html).not.toContain('已整理至事件 #15')
    const legacy = render({ ...saved, pending: true, source_cursor: null })
    expect(legacy).toContain('旧版记忆等待完整补读')
    expect(legacy).not.toContain('已整理至事件 #15')
  })
})
