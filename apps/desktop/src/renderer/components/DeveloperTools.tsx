import { useCreateRun } from '../lib/use-create-run'
import { errorMessage } from '../lib/format'
import { InlineError } from './Feedback'

export function DeveloperTools() {
  const demo = useCreateRun()
  return (
    <details className="settings-section developer-tools">
      <summary>开发调试</summary>
      <p>使用预设数据验证执行、压缩与恢复，不调用真实模型，也不操作项目文件。</p>
      <button
        className="model-save"
        disabled={demo.isPending}
        onClick={() =>
          demo.mutate({
            model_id: 'local-demo',
            prompt:
              '请运行上下文诊断演示，让我观察工具输出如何进入上下文，以及达到阈值后的压缩过程。',
          })
        }
      >
        {demo.isPending ? '正在启动…' : '运行上下文演示'}
      </button>
      {demo.isError && <InlineError message={errorMessage(demo.error)} />}
    </details>
  )
}
