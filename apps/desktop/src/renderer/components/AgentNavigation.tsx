import { createContext, useContext, type ReactNode } from 'react'
import type { RunSummary } from '../../shared/contracts'

interface AgentNavigation {
  runs: RunSummary[]
  selectedId: string | null
  openAgent: (id: string) => void
}

const Context = createContext<AgentNavigation | null>(null)

export function AgentNavigationProvider({
  value,
  children,
}: {
  value: AgentNavigation
  children: ReactNode
}) {
  return <Context.Provider value={value}>{children}</Context.Provider>
}

export function useAgentNavigation(): AgentNavigation | null {
  return useContext(Context)
}
