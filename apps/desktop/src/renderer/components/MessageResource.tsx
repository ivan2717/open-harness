import { useEffect, useState, type ReactNode } from 'react'
import {
  Download,
  ExternalLink,
  FileText,
  FolderOpen,
  ImageIcon,
  LoaderCircle,
  Play,
} from 'lucide-react'
import type { ResourceReference, ResolvedResource } from '../../shared/message-resources'
import { MessagePreviewDialog } from './MessagePreviewDialog'
import { resourceName } from '../lib/message-links'

type ResourceState = { resource?: ResolvedResource; error?: string }

function useResource(reference: ResourceReference, enabled: boolean) {
  const [state, setState] = useState<ResourceState>({})
  const [attempt, setAttempt] = useState(0)
  const key = JSON.stringify(reference)
  useEffect(() => {
    let active = true
    setState({})
    if (!enabled) return
    void window.harness.resolveResource(JSON.parse(key) as ResourceReference).then(
      (resource) => {
        if (active) setState({ resource })
      },
      (error: unknown) => {
        if (active) setState({ error: error instanceof Error ? error.message : '无法读取文件' })
      },
    )
    return () => {
      active = false
    }
  }, [key, enabled, attempt])
  return { ...state, retry: () => setAttempt((value) => value + 1) }
}

function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function ResourceActions({ resource }: { resource: ResolvedResource }) {
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  async function act(action: 'open' | 'reveal' | 'save') {
    setPending(true)
    setError('')
    try {
      await window.harness.resourceAction(resource.id, action)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : '操作失败，请重试')
    } finally {
      setPending(false)
    }
  }
  return (
    <span className="message-resource-actions">
      {resource.canOpen !== false && (
        <button type="button" disabled={pending} onClick={() => void act('open')}>
          <ExternalLink size={13} />
          打开
        </button>
      )}
      <button type="button" disabled={pending} onClick={() => void act('reveal')}>
        <FolderOpen size={13} />
        显示位置
      </button>
      <button type="button" disabled={pending} onClick={() => void act('save')}>
        <Download size={13} />
        另存为
      </button>
      {error && (
        <span className="message-resource-error" role="alert">
          {error}
        </span>
      )}
    </span>
  )
}

function ResourceContent({
  resource,
  expanded = false,
}: {
  resource: ResolvedResource
  expanded?: boolean
}) {
  const [failed, setFailed] = useState(false)
  if (failed)
    return (
      <span className="message-resource-error" role="status">
        无法预览此媒体，可打开或另存为。
      </span>
    )
  if (resource.kind === 'image')
    return (
      <img
        src={resource.url}
        alt={resource.name}
        loading={expanded ? 'eager' : 'lazy'}
        onError={() => setFailed(true)}
      />
    )
  if (resource.kind === 'video')
    return (
      <video
        src={resource.url}
        controls
        preload="none"
        aria-label={resource.name}
        onError={() => setFailed(true)}
      />
    )
  if (resource.kind === 'audio')
    return (
      <audio
        src={resource.url}
        controls
        preload="none"
        aria-label={resource.name}
        onError={() => setFailed(true)}
      />
    )
  if (expanded && resource.kind === 'text')
    return <pre className="message-file-text">{resource.text ?? '没有可预览的文本。'}</pre>
  if (expanded)
    return <p className="message-file-unavailable">此文件暂不支持内嵌预览，可以打开或另存为。</p>
  return null
}

function ResourcePreview({ resource, close }: { resource: ResolvedResource; close: () => void }) {
  return (
    <MessagePreviewDialog
      title={resource.name}
      onClose={close}
      actions={<ResourceActions resource={resource} />}
    >
      <ResourceContent resource={resource} expanded />
    </MessagePreviewDialog>
  )
}

export function LocalResourceLink({
  reference,
  children,
  enabled = true,
  className = 'message-file-link',
  ariaLabel,
}: {
  reference: ResourceReference
  children: ReactNode
  enabled?: boolean
  className?: string
  ariaLabel?: string
}) {
  const { resource, error, retry } = useResource(reference, enabled)
  const [show, setShow] = useState(false)
  return (
    <>
      <button
        type="button"
        className={className}
        aria-label={ariaLabel}
        title={error || (enabled ? '预览文件' : '生成完成后可预览')}
        disabled={!enabled || (!resource && !error)}
        onClick={() => {
          if (error) retry()
          else setShow(true)
        }}
      >
        {children}
      </button>
      {error && (
        <span className="message-resource-error" role="status">
          {error}
        </span>
      )}
      {show && resource && <ResourcePreview resource={resource} close={() => setShow(false)} />}
    </>
  )
}

export function LocalMessageResource({
  reference,
  label,
  enabled = true,
}: {
  reference: ResourceReference
  label: string
  enabled?: boolean
}) {
  const { resource, error, retry } = useResource(reference, enabled)
  const [show, setShow] = useState(false)
  if (!enabled)
    return (
      <span className="message-resource-placeholder">
        <FileText size={15} />
        {label} · 生成完成后可预览
      </span>
    )
  if (error)
    return (
      <span className="message-resource-placeholder">
        <FileText size={15} />
        <span>
          {label}
          <small role="status">{error}</small>
        </span>
        <button type="button" onClick={retry}>
          重试
        </button>
      </span>
    )
  if (!resource)
    return (
      <span className="message-resource-placeholder">
        <LoaderCircle size={15} className="animate-spin" />
        正在读取 {label}…
      </span>
    )
  const media = resource.kind === 'image' || resource.kind === 'video' || resource.kind === 'audio'
  return (
    <span className={`message-resource message-resource-${resource.kind}`}>
      {media &&
        (resource.kind === 'image' ? (
          <button
            type="button"
            className="message-image-button"
            aria-label={`放大查看 ${resource.name}`}
            onClick={() => setShow(true)}
          >
            <ResourceContent resource={resource} />
          </button>
        ) : (
          <ResourceContent resource={resource} />
        ))}
      <span className="message-resource-caption">
        <button type="button" className="message-resource-name" onClick={() => setShow(true)}>
          <FileText size={15} />
          <span>{label || resource.name}</span>
        </button>
        <small>{sizeLabel(resource.size)}</small>
      </span>
      <ResourceActions resource={resource} />
      {show && <ResourcePreview resource={resource} close={() => setShow(false)} />}
    </span>
  )
}

export function ExternalMessageLink({ url, children }: { url: string; children: ReactNode }) {
  const [error, setError] = useState('')
  return (
    <>
      <a
        href={url}
        rel="noreferrer noopener"
        onClick={(event) => {
          event.preventDefault()
          setError('')
          void window.harness.openExternal(url).catch((cause: unknown) => {
            setError(cause instanceof Error ? cause.message : '无法打开链接')
          })
        }}
      >
        {children}
      </a>
      {error && (
        <span className="message-resource-error" role="alert">
          {error}
        </span>
      )}
    </>
  )
}

export function RemoteMessageMedia({
  url,
  label,
  kind,
  enabled = true,
}: {
  url: string
  label: string
  kind: 'image' | 'video' | 'audio'
  enabled?: boolean
}) {
  const [loaded, setLoaded] = useState(false)
  const [failed, setFailed] = useState(false)
  const [expanded, setExpanded] = useState(false)
  const name = label || resourceName(url)
  const media =
    kind === 'image' ? (
      <img src={url} alt={name} referrerPolicy="no-referrer" onError={() => setFailed(true)} />
    ) : kind === 'video' ? (
      <video src={url} controls preload="none" aria-label={name} onError={() => setFailed(true)} />
    ) : (
      <audio src={url} controls preload="none" aria-label={name} onError={() => setFailed(true)} />
    )
  return (
    <span className={`message-resource message-resource-${kind}`}>
      {!enabled ? (
        <span className="message-resource-placeholder">{name} · 生成完成后可加载</span>
      ) : !loaded ? (
        <button type="button" className="message-remote-load" onClick={() => setLoaded(true)}>
          {kind === 'image' ? <ImageIcon size={19} /> : <Play size={19} />}
          <span>
            加载{kind === 'image' ? '图片' : kind === 'video' ? '视频' : '音频'}
            <small>{new URL(url).hostname}</small>
          </span>
        </button>
      ) : failed ? (
        <span className="message-resource-error" role="status">
          无法加载此媒体，请通过原链接打开。
        </span>
      ) : kind === 'image' ? (
        <button
          type="button"
          className="message-image-button"
          aria-label={`放大查看 ${name}`}
          onClick={() => setExpanded(true)}
        >
          {media}
        </button>
      ) : (
        media
      )}
      <span className="message-resource-caption">
        <ExternalMessageLink url={url}>{name}</ExternalMessageLink>
      </span>
      {expanded && (
        <MessagePreviewDialog title={name} onClose={() => setExpanded(false)}>
          {media}
        </MessagePreviewDialog>
      )}
    </span>
  )
}
