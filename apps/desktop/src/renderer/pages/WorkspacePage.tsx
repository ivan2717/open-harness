import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { ArrowRight, ChevronRight, FlaskConical, PanelRightOpen, Plus } from 'lucide-react'
import type { CreateRunInput } from '../../shared/contracts'
import { Composer } from '../components/Composer'
import { ContextInspector } from '../components/ContextInspector'
import { Conversation } from '../components/Conversation'
import { ErrorState, InlineError, Loading } from '../components/Feedback'
import { errorMessage, statusLabels } from '../lib/format'
import { configOptions, storeSnapshot, useRun } from '../lib/queries'
import { useUiStore } from '../state/ui-store'

function useCreateRun() {
  const client = useQueryClient()
  const navigate = useNavigate()
  return useMutation({
    mutationFn: (input: CreateRunInput) => window.harness.createRun(input),
    onSuccess: (run) => {
      storeSnapshot(client, run)
      void navigate({ to: '/runs/$runId', params: { runId: run.id } })
    },
  })
}

function InspectorToggle() {
  const open = useUiStore((state) => state.inspectorOpen)
  const setOpen = useUiStore((state) => state.setInspectorOpen)
  return !open ? (
    <button
      className="inspector-toggle"
      aria-label="打开运行详情"
      title="打开运行详情"
      onClick={() => setOpen(true)}
    >
      <PanelRightOpen size={17} />
      运行详情
    </button>
  ) : null
}

export function WelcomePage() {
  const config = useQuery(configOptions)
  const create = useCreateRun()
  const selectedModel = useUiStore((state) => state.selectedModel)
  const setModel = useUiStore((state) => state.setSelectedModel)
  const setDraft = useUiStore((state) => state.setDraft)
  if (config.isPending) return <Loading />
  if (config.isError) return <ErrorState error={config.error} retry={() => void config.refetch()} />
  const modelId = config.data.models.some((model) => model.id === selectedModel && model.available)
    ? selectedModel
    : config.data.default_model
  const model = config.data.models.find((entry) => entry.id === modelId)
  return (
    <div className="workspace-layout">
      <section className="main-pane">
        <div className="welcome-scroll">
          <div className="welcome-content">
            <h1>开始一个新任务</h1>
            <div className="welcome-composer">
              <Composer
                draftKey="new"
                models={config.data.models}
                modelId={modelId}
                onModelChange={setModel}
                pending={create.isPending}
                disabled={!model?.available}
                onSubmit={async (prompt) => {
                  await create.mutateAsync({ prompt, model_id: modelId })
                }}
              />
              {create.isError && <InlineError message={errorMessage(create.error)} />}
            </div>
            {modelId === 'local-demo' && (
              <button
                className="demo-shortcut"
                disabled={create.isPending}
                onClick={() => {
                  setDraft(
                    'new',
                    '请运行上下文诊断演示，让我观察工具输出如何进入上下文，以及达到阈值后的压缩过程。',
                  )
                  document.querySelector<HTMLTextAreaElement>('.composer textarea')?.focus()
                }}
              >
                试试上下文演示
                <ArrowRight size={13} />
              </button>
            )}
          </div>
        </div>
      </section>
    </div>
  )
}

export function RunPage({ runId }: { runId: string }) {
  const query = useRun(runId)
  const config = useQuery(configOptions)
  const client = useQueryClient()
  const create = useCreateRun()
  const inspectorOpen = useUiStore((state) => state.inspectorOpen)
  const cancel = useMutation({
    mutationFn: () => window.harness.cancelRun(runId),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  const input = useMutation({
    mutationFn: (message: string) => window.harness.submitInput(runId, message),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  if (query.isPending) return <Loading label="正在恢复会话记录" />
  if (query.isError && !query.data)
    return <ErrorState error={query.error} retry={() => void query.refetch()} />
  const run = query.data
  if (!run) return null
  const running = run.status === 'running'
  const waiting = run.status === 'waiting_for_input'
  const mutationError = cancel.error ?? input.error ?? create.error
  const model = config.data?.models.find((entry) => entry.id === run.model_id)
  return (
    <div className="workspace-layout">
      <section className="main-pane">
        <header className="workspace-header">
          <div className="min-w-0 flex-1">
            <div className="workspace-heading">
              <span className={`status-dot status-${run.status}`} />
              <span className="truncate">{run.title || run.prompt}</span>
            </div>
            <div className="run-header-meta">
              <span>{model?.name ?? run.model_id}</span>
              <ChevronRight size={10} />
              <span>{statusLabels[run.status]}</span>
            </div>
          </div>
          <div className="header-actions">
            <span className={`run-status-label status-text-${run.status}`}>
              {statusLabels[run.status]}
            </span>
            <InspectorToggle />
          </div>
        </header>
        {query.streamError && (
          <div className="run-alert">
            <InlineError
              message={`实时连接中断，正在定期刷新。${query.streamError}`}
              retry={query.reconnect}
            />
          </div>
        )}
        {run.model_id === 'local-demo' && (
          <div className="run-demo-notice">
            <FlaskConical size={12} />
            离线演示 · 无模型 API 调用 · 工具数据为演示内容
          </div>
        )}
        <Conversation run={run} />
        <div className="run-composer-area">
          {run.error && <InlineError message={run.error} />}
          {mutationError && <InlineError message={errorMessage(mutationError)} />}
          {!running && !waiting && (
            <div className="finished-note">
              <Plus size={12} />
              本次运行已结束，发送新任务将创建独立会话。
            </div>
          )}
          <Composer
            draftKey={`run:${runId}`}
            models={config.data?.models ?? []}
            modelId={run.model_id}
            running={running}
            onStop={() => cancel.mutate()}
            stopping={cancel.isPending}
            waiting={waiting}
            pending={input.isPending || create.isPending}
            disabled={!running && !waiting && !model?.available}
            onSubmit={async (prompt) => {
              if (waiting) await input.mutateAsync(prompt)
              else await create.mutateAsync({ prompt, model_id: run.model_id })
            }}
          />
          <p className="composer-bottom-note">
            {running ? '流式内容提交后会保存到运行记录。' : 'Enter 发送 · Shift + Enter 换行'}
          </p>
        </div>
      </section>
      {inspectorOpen && <ContextInspector run={run} windowTokens={model?.context_window} />}
    </div>
  )
}
