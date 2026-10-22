import { LoaderCircle } from 'lucide-react'

export function RunActivity({ active }: { active: boolean }) {
  return (
    <span className="run-activity" role="img" aria-label={active ? '运行中' : '空闲'}>
      {active ? (
        <LoaderCircle size={12} className="animate-spin" aria-hidden="true" />
      ) : (
        <span className="run-idle-dot" />
      )}
    </span>
  )
}
