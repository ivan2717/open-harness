import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { MessageMarkdown } from './MessageMarkdown'

function render(text: string, settled = true): string {
  return renderToStaticMarkup(
    <MessageMarkdown text={text} runId="run-1" messageId="event-2" settled={settled} />,
  )
}

describe('assistant Markdown presentation', () => {
  it('renders headings, emphasis, GFM tables and fenced code with copy controls', () => {
    const html = render(
      '## 烛影\n\n**粗体**和`code`\n\n| 名称 | 状态 |\n| --- | --- |\n| 文件 | 完成 |\n\n```ts\nconst answer = 42\n```',
    )
    expect(html).toContain('<h2>烛影</h2>')
    expect(html).toContain('<strong>粗体</strong>')
    expect(html).toContain('<table>')
    expect(html).toContain('aria-label="复制代码"')
    expect(html).toContain('hljs-keyword')
  })

  it('keeps HTML inert and never emits dangerous links or hidden network images', () => {
    const html = render(
      '<script>alert(1)</script>\n\n[bad](javascript:alert)\n\n![remote](https://example.com/a.png)\n\n![unsafe](file:///private/secret.png)',
    )
    expect(html).not.toContain('<script>')
    expect(html).toContain('&lt;script&gt;')
    expect(html).not.toContain('href="javascript:')
    expect(html).not.toContain('<img')
    expect(html).not.toContain('src=')
    expect(html).toContain('加载图片')
    expect(html).toContain('不支持的资源地址')
  })

  it('renders partial fences while keeping resources inactive during streaming', () => {
    const html = render('![plot](./plot.png)\n\n```ts\nconst partial =', false)
    expect(html).toContain('const partial =')
    expect(html).toContain('生成完成后可预览')
    expect(html).not.toContain('<img')
    expect(html).not.toContain('hljs-keyword')
  })

  it('routes local links to resource cards and safe external links to the desktop bridge', () => {
    const html = render('[报告](./report.txt)\n\nSee [docs](https://example.com/docs).')
    expect(html).toContain('正在读取 报告')
    expect(html).not.toContain('href="./report.txt"')
    expect(html).toContain('href="https://example.com/docs"')
    expect(html).toContain('rel="noreferrer noopener"')
  })

  it('does not fail when the model names an unknown code language', () => {
    expect(() => render('```imaginary-language\nHello\n```')).not.toThrow()
  })

  it('keeps linked media separate from the surrounding link action', () => {
    const html = render('See [![image](https://example.com/image.png)](https://example.com/page).')
    expect(html).toContain('加载图片')
    expect(html).toContain('打开链接</a>')
    expect(html).not.toMatch(/<a[^>]*>[\s\S]*<button[\s\S]*<\/a>/)
  })
})
