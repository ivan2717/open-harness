import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useState } from 'react'
import {
  ArrowLeft,
  FlaskConical,
  PanelRightOpen,
  FileDiff,
  FolderOpen,
  Play,
  Minimize2,
  Square,
} from 'lucide-react'
import { Composer } from '../components/Composer'
import { RunActivity } from '../components/RunActivity'
import { ContextInspector } from '../components/ContextInspector'
import { Conversation } from '../components/Conversation'
import { CollaborationTasks } from '../components/CollaborationTasks'
import {
  CommandApprovals,
  RunningCommands,
  WorkspaceReviewPanel,
} from '../components/WorkspaceReview'
import { ExtensionApprovals } from '../components/ExtensionApprovals'
import { ErrorState, InlineError, Loading } from '../components/Feedback'
import { errorMessage } from '../lib/format'
import { pauseMessage } from '../lib/collaboration'
import { configOptions, storeSnapshot, useRun, useWorkspace } from '../lib/queries'
import { taskModels } from '../lib/task-models'
import { useCreateRun } from '../lib/use-create-run'
import { useUiStore } from '../state/ui-store'
import type {
  CommandApprovalScope,
  FileReference,
  UpdateRunModelInput,
} from '../../shared/contracts'

interface UserMessageInput {
  message?: string
  files?: FileReference[]
}

function InspectorToggle({ onOpen }: { onOpen?: () => void }) {
  const open = useUiStore((state) => state.inspectorOpen)
  const setOpen = useUiStore((state) => state.setInspectorOpen)
  return !open ? (
    <button
      className="inspector-toggle"
      aria-label="打开运行详情"
      title="打开运行详情"
      onClick={() => {
        onOpen?.()
        setOpen(true)
      }}
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
  const draftEfforts = useUiStore((state) => state.draftReasoningEfforts)
  const setDraftEffort = useUiStore((state) => state.setDraftReasoningEffort)
  const project = useUiStore((state) => state.project)
  const setProject = useUiStore((state) => state.setProject)
  if (config.isPending) return <Loading />
  if (config.isError) return <ErrorState error={config.error} retry={() => void config.refetch()} />
  const { models, selected: model } = taskModels(config.data, selectedModel)
  const savedEffort = model ? draftEfforts[model.id] : undefined
  const reasoningEffort =
    savedEffort && model?.reasoning?.supported_efforts.includes(savedEffort)
      ? savedEffort
      : undefined
  const draftKey = project ? `new:${project.path}` : 'new'
  return (
    <div className="workspace-layout">
      <section className="main-pane">
        <div className="welcome-scroll">
          <div className="welcome-content">
            <h1>开始一个新任务</h1>
            <div className="welcome-composer">
              <Composer
                key={draftKey}
                autoFocus
                draftKey={draftKey}
                models={models}
                modelId={model?.id ?? ''}
                onModelChange={setModel}
                reasoningEffort={reasoningEffort}
                onReasoningChange={(effort) => {
                  if (model) setDraftEffort(model.id, effort)
                }}
                project={project}
                onProjectChange={setProject}
                pending={create.isPending}
                submitDisabledReason={!model ? '添加模型后即可开始任务。' : undefined}
                onSubmit={async (prompt, files) => {
                  if (!model) throw new Error('添加模型后即可开始任务。')
                  await create.mutateAsync({
                    prompt,
                    model_id: model.id,
                    ...(reasoningEffort ? { reasoning_effort: reasoningEffort } : {}),
                    files,
                    ...(project
                      ? { workspace_path: project.path, permission_mode: project.permission }
                      : {}),
                  })
                  if (useUiStore.getState().project === project) setProject(null)
                }}
              />
              {create.isError && <InlineError message={errorMessage(create.error)} />}
            </div>
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
  const inspectorOpen = useUiStore((state) => state.inspectorOpen)
  const setInspectorOpen = useUiStore((state) => state.setInspectorOpen)
  const [reviewRun, setReviewRun] = useState<string | null>(null)
  const [reviewTab, setReviewTab] = useState<'changes' | 'commands'>('changes')
  const workspace = useWorkspace(
    runId,
    Boolean(query.data?.workspace_path || query.data?.execution_directory),
    query.data?.status === 'running',
  )
  const refreshWorkspace = () => void client.invalidateQueries({ queryKey: ['workspace', runId] })
  const resume = useMutation({
    mutationFn: ({ message, files }: UserMessageInput) =>
      window.harness.resumeRun(runId, message, files),
    onSuccess: (run) => {
      storeSnapshot(client, run)
      refreshWorkspace()
    },
  })
  const approve = useMutation({
    mutationFn: ({
      id,
      allow,
      scope,
    }: {
      id: string
      allow: boolean
      scope?: CommandApprovalScope
    }) => window.harness.approveCommand(runId, id, allow, scope),
    onSuccess: (review) => client.setQueryData(['workspace', runId], review),
    onSettled: refreshWorkspace,
  })
  const revokeGrants = useMutation({
    mutationFn: () => window.harness.revokeCommandGrants(runId),
    onSuccess: (review) => client.setQueryData(['workspace', runId], review),
    onSettled: refreshWorkspace,
  })
  const revert = useMutation({
    mutationFn: (path: string) => window.harness.revertFile(runId, path),
    onSuccess: (review) => client.setQueryData(['workspace', runId], review),
    onSettled: refreshWorkspace,
  })
  const stopCommand = useMutation({
    mutationFn: (commandId: string) => window.harness.stopCommand(runId, commandId),
    onSuccess: (review) => client.setQueryData(['workspace', runId], review),
    onSettled: refreshWorkspace,
  })
  const revealWorkspace = useMutation({ mutationFn: () => window.harness.revealWorkspace(runId) })
  const cancel = useMutation({
    mutationFn: () => window.harness.cancelRun(runId),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  const input = useMutation({
    mutationFn: ({ message, files }: UserMessageInput) =>
      window.harness.submitInput(runId, message ?? '', files),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  const compact = useMutation({
    mutationFn: () => window.harness.compactRun(runId),
    onSuccess: (run) => storeSnapshot(client, run),
    onSettled: () => {
      void client.invalidateQueries({ queryKey: ['run', runId] })
    },
  })
  const cancelCompaction = useMutation({
    mutationFn: () => window.harness.cancelCompaction(runId),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  const modelSettings = useMutation({
    mutationFn: (settings: UpdateRunModelInput) => window.harness.updateRunModel(runId, settings),
    onSuccess: (run) => storeSnapshot(client, run),
  })
  if (query.isPending) return <Loading label="正在恢复会话记录" />
  if (query.isError && !query.data)
    return <ErrorState error={query.error} retry={() => void query.refetch()} />
  const run = query.data
  if (!run) return null
  const running = run.status === 'running'
  const waiting = run.status === 'waiting_for_input'
  const delegated = Boolean(run.delegation)
  const hasWorkspace = Boolean(run.workspace_path || run.execution_directory)
  const compacting = run.compacting || compact.isPending
  const active =
    running || compacting || input.isPending || resume.isPending || modelSettings.isPending
  const mutationError =
    cancel.error ??
    input.error ??
    resume.error ??
    compact.error ??
    cancelCompaction.error ??
    approve.error ??
    stopCommand.error ??
    revealWorkspace.error ??
    modelSettings.error
  const model = config.data?.models.find((entry) => entry.id === run.model_id)
  const models = (config.data?.models ?? []).filter((entry) =>
    run.model_id === 'local-demo' ? entry.id === 'local-demo' : entry.id !== 'local-demo',
  )
  return (
    <div className="workspace-layout">
      <section className="main-pane">
        <header className="workspace-header">
          <div className="min-w-0 flex-1">
            {run.delegation && (
              <Link
                to="/runs/$runId"
                params={{ runId: run.delegation.parent_run_id }}
                className="delegation-back"
              >
                <ArrowLeft size={12} aria-hidden="true" />
                返回父任务
              </Link>
            )}
            <div className="workspace-heading">
              <RunActivity active={Boolean(active)} />
              <span className="truncate">{run.title || run.prompt}</span>
            </div>
            <div className="run-header-meta">
              <span>{model?.name ?? run.model_id}</span>
            </div>
            {run.workspace_path && (
              <div className="run-project-meta" title={run.workspace_path}>
                <FolderOpen size={12} />
                <span className="truncate">{run.workspace_path}</span>
                <span>{run.permission_mode === 'workspace_write' ? '允许修改' : '只读'}</span>
              </div>
            )}
          </div>
          <div className="header-actions">
            {hasWorkspace && (
              <button
                className="inspector-toggle"
                aria-label={run.workspace_path ? '查看项目结果' : '查看会话文件与命令'}
                aria-expanded={reviewRun === runId}
                onClick={() => {
                  setReviewRun(reviewRun === runId ? null : runId)
                  setReviewTab('changes')
                  setInspectorOpen(false)
                }}
              >
                <FileDiff size={15} />
                {run.workspace_path ? '改动与命令' : '文件与命令'}{' '}
                {workspace.data?.changes.length || ''}
              </button>
            )}
            <InspectorToggle onOpen={() => setReviewRun(null)} />
          </div>
        </header>
        {!delegated && <CollaborationTasks key={runId} rootId={runId} />}
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
          <ExtensionApprovals key={runId} runId={runId} running={running} busy={cancel.isPending} />
          <CommandApprovals
            approvals={workspace.data?.approvals ?? []}
            busy={approve.isPending || cancel.isPending}
            onDecide={(id, allow, scope) => approve.mutate({ id, allow, scope })}
          />
          <RunningCommands
            commands={workspace.data?.commands ?? []}
            stoppingId={stopCommand.isPending ? stopCommand.variables : undefined}
            onInspect={() => {
              setReviewRun(runId)
              setReviewTab('commands')
              setInspectorOpen(false)
            }}
            onStop={(id) => stopCommand.mutate(id)}
          />
          {hasWorkspace && workspace.isError && (
            <InlineError
              message={`文件与命令状态读取失败。${errorMessage(workspace.error)}`}
              retry={() => void workspace.refetch()}
            />
          )}
          {run.error && run.status !== 'paused' && <InlineError message={run.error} />}
          {mutationError && <InlineError message={errorMessage(mutationError)} />}
          {run.status === 'paused' && (
            <div className="run-pause-note" role="status">
              <span>{pauseMessage(run)}</span>
              {!delegated && (
                <button
                  className="resume-button"
                  disabled={
                    resume.isPending || modelSettings.isPending || compacting || !model?.available
                  }
                  onClick={() => resume.mutate({})}
                >
                  <Play size={12} />
                  继续任务
                </button>
              )}
            </div>
          )}
          {!delegated && !running && !waiting && !['completed', 'paused'].includes(run.status) && (
            <div className="finished-note">
              {run.status === 'interrupted' && <span>执行已中断，消息已保存。</span>}
              <button
                className="resume-button"
                disabled={
                  resume.isPending || modelSettings.isPending || compacting || !model?.available
                }
                onClick={() => resume.mutate({})}
              >
                <Play size={12} />
                继续任务
              </button>
            </div>
          )}
          {run.delegation ? (
            <div className="delegation-footer">
              <span>在主任务中继续协作。</span>
              {running && (
                <button
                  type="button"
                  className="resume-button"
                  disabled={cancel.isPending}
                  onClick={() => cancel.mutate()}
                >
                  <Square size={12} />
                  {cancel.isPending ? '正在停止…' : '停止任务'}
                </button>
              )}
            </div>
          ) : (
            <>
              <Composer
                key={runId}
                runId={runId}
                draftKey={`run:${runId}`}
                models={models}
                modelId={run.model_id}
                reasoningEffort={run.reasoning_effort}
                onModelChange={(modelId) => {
                  if (modelId !== run.model_id)
                    modelSettings.mutate({ model_id: modelId, reasoning_effort: null })
                }}
                onReasoningChange={(effort) => {
                  if (effort !== (run.reasoning_effort ?? null))
                    modelSettings.mutate({ model_id: run.model_id, reasoning_effort: effort })
                }}
                modelSettingsDisabled={compacting}
                modelSettingsDisabledReason={
                  compacting ? '正在整理上下文，完成后可切换模型和思考强度。' : undefined
                }
                project={
                  run.workspace_path
                    ? {
                        path: run.workspace_path,
                        permission: run.permission_mode ?? 'read_only',
                      }
                    : null
                }
                executionDirectory={run.execution_directory}
                onRevealWorkspace={() => revealWorkspace.mutate()}
                running={running}
                onStop={() => cancel.mutate()}
                stopping={cancel.isPending}
                pending={input.isPending || resume.isPending || modelSettings.isPending}
                disabled={compacting || (!running && !model?.available)}
                onSubmit={async (prompt, files) => {
                  if (waiting) await input.mutateAsync({ message: prompt, files })
                  else await resume.mutateAsync({ message: prompt, files })
                }}
              />
              <p className="composer-bottom-note">Enter 发送 · Shift + Enter 换行</p>
            </>
          )}
        </div>
      </section>
      {inspectorOpen && (
        <ContextInspector
          run={run}
          windowTokens={model?.context_window}
          compactionAction={
            !delegated && (
              <button
                type="button"
                className="inspector-toggle"
                disabled={
                  input.isPending ||
                  resume.isPending ||
                  modelSettings.isPending ||
                  (compacting
                    ? cancelCompaction.isPending
                    : running ||
                      !run.messages.length ||
                      !['waiting_for_input', 'completed'].includes(run.status) ||
                      !model?.available)
                }
                title={
                  compacting
                    ? '取消当前的上下文压缩。'
                    : '汇总较早的历史，保留原始记录。真实模型会产生摘要请求。'
                }
                onClick={() => (compacting ? cancelCompaction.mutate() : compact.mutate())}
              >
                <Minimize2 size={15} />
                {compacting ? '取消压缩' : '压缩上下文'}
              </button>
            )
          }
        />
      )}
      {hasWorkspace && reviewRun === runId && (
        <WorkspaceReviewPanel
          key={`${runId}-${reviewTab}`}
          initialTab={reviewTab}
          managed={!run.workspace_path}
          review={workspace.data}
          loading={workspace.isPending}
          error={
            revert.error || revokeGrants.error
              ? errorMessage(revokeGrants.error ?? revert.error)
              : undefined
          }
          busy={running || revert.isPending}
          onClose={() => setReviewRun(null)}
          onRefresh={() => void workspace.refetch()}
          onRevert={(path) => revert.mutateAsync(path)}
          onStopCommand={(id) => stopCommand.mutate(id)}
          stoppingCommandId={stopCommand.isPending ? stopCommand.variables : undefined}
          onRevealWorkspace={() => revealWorkspace.mutate()}
          onRevokeGrants={() => revokeGrants.mutate()}
          revokingGrants={revokeGrants.isPending}
        />
      )}
    </div>
  )
}
