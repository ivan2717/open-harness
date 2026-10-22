import { Children, isValidElement, memo, useMemo, useState, type ReactNode } from 'react'
import Markdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import rehypeHighlight from 'rehype-highlight'
import { Check, Copy } from 'lucide-react'
import type { Element, Nodes } from 'hast'
import { messageLink, resourceName } from '../lib/message-links'
import {
  ExternalMessageLink,
  LocalMessageResource,
  LocalResourceLink,
  RemoteMessageMedia,
} from './MessageResource'
import '../message-content.css'

function plainText(children: ReactNode): string {
  return Children.toArray(children)
    .map((child) => {
      if (typeof child === 'string' || typeof child === 'number') return String(child)
      return isValidElement<{ children?: ReactNode }>(child) ? plainText(child.props.children) : ''
    })
    .join('')
}

function astText(node: Nodes): string {
  if (node.type === 'text') return node.value
  return 'children' in node ? node.children.map(astText).join('') : ''
}

function CodeBlock({ children }: { children: ReactNode }) {
  const [copied, setCopied] = useState(false)
  const [error, setError] = useState('')
  const code = Children.toArray(children).find((child) => isValidElement(child))
  const className = isValidElement<{ className?: string }>(code) ? code.props.className : ''
  const language = /(?:^|\s)language-([^\s]+)/.exec(className ?? '')?.[1]
  return (
    <div className="message-code-block">
      <div className="message-code-toolbar">
        <span>{language || '代码'}</span>
        <button
          type="button"
          aria-label="复制代码"
          onClick={() => {
            setError('')
            void window.harness.copyText(plainText(children)).then(
              () => setCopied(true),
              () => setError('复制失败，请手动选择代码复制'),
            )
          }}
          onBlur={() => setCopied(false)}
        >
          {copied ? <Check size={13} /> : <Copy size={13} />}
          {copied ? '已复制' : '复制'}
        </button>
      </div>
      <pre>{children}</pre>
      {error && (
        <span className="message-resource-error" role="alert">
          {error}
        </span>
      )}
    </div>
  )
}

function loneLink(node: Element | undefined): Element | undefined {
  const children = node?.children.filter((child) => child.type !== 'text' || child.value.trim())
  const child = children?.length === 1 ? children[0] : undefined
  return child?.type === 'element' && child.tagName === 'a' && !containsImage(child)
    ? child
    : undefined
}

function containsImage(node: Nodes | undefined): boolean {
  if (node?.type === 'element' && node.tagName === 'img') return true
  return Boolean(node && 'children' in node && node.children.some(containsImage))
}

export const MessageMarkdown = memo(function MessageMarkdown({
  text,
  runId,
  messageId,
  settled,
}: {
  text: string
  runId: string
  messageId: string
  settled: boolean
}) {
  const components = useMemo<Components>(
    () => ({
      a({ href, children, node }) {
        const link = messageLink(href ?? '')
        // Image previews have their own buttons. Keep a surrounding Markdown
        // link alongside the preview instead of nesting interactive controls.
        const media = containsImage(node)
        const label = media ? '打开链接' : children
        let action: ReactNode = <span>{label}</span>
        if (link.kind === 'external')
          action = <ExternalMessageLink url={link.url}>{label}</ExternalMessageLink>
        else if (link.kind === 'local')
          action = (
            <LocalResourceLink
              reference={{ runId, messageId, source: link.source }}
              enabled={settled}
            >
              {label}
            </LocalResourceLink>
          )
        return media ? (
          <span>
            {children}
            {action}
          </span>
        ) : (
          action
        )
      },
      img({ src, alt }) {
        const source = typeof src === 'string' ? src : ''
        const link = messageLink(source)
        const label = alt || resourceName(source) || '图片'
        if (link.kind === 'external' && /^https?:/i.test(link.url))
          return (
            <RemoteMessageMedia
              url={link.url}
              label={label}
              kind={link.media ?? 'image'}
              enabled={settled}
            />
          )
        if (link.kind === 'local')
          return (
            <LocalMessageResource
              reference={{ runId, messageId, source: link.source }}
              label={label}
              enabled={settled}
            />
          )
        return <span className="message-resource-placeholder">{label} · 不支持的资源地址</span>
      },
      p({ node, children }) {
        const only = loneLink(node)
        if (only) {
          const source = String(only.properties.href ?? '')
          const link = messageLink(source)
          const label = astText(only) || resourceName(source)
          if (link.kind === 'local')
            return (
              <div className="message-resource-paragraph">
                <LocalMessageResource
                  reference={{ runId, messageId, source: link.source }}
                  label={label}
                  enabled={settled}
                />
              </div>
            )
          if (link.kind === 'external' && link.media)
            return (
              <div className="message-resource-paragraph">
                <RemoteMessageMedia
                  url={link.url}
                  label={label}
                  kind={link.media}
                  enabled={settled}
                />
              </div>
            )
        }
        return <p>{children}</p>
      },
      pre({ children }) {
        return <CodeBlock>{children}</CodeBlock>
      },
      table({ children }) {
        return (
          <div className="message-table-scroll" tabIndex={0} role="region" aria-label="表格">
            <table>{children}</table>
          </div>
        )
      },
    }),
    [runId, messageId, settled],
  )

  return (
    <div className="message-markdown">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={
          settled && text.length <= 64_000
            ? [[rehypeHighlight, { detect: false, ignoreMissing: true }]]
            : []
        }
        // Raw HTML remains literal. In particular, it can never create an iframe,
        // script, event handler, or unmediated local/remote resource request.
        urlTransform={(url) => (messageLink(url).kind === 'blocked' ? '' : url)}
        components={components}
      >
        {text}
      </Markdown>
    </div>
  )
})
