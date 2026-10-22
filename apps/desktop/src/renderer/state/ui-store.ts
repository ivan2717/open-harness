import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { PermissionMode } from '../../shared/contracts'

export type InspectorTab = 'context' | 'timeline'

export interface ProjectSelection {
  path: string
  permission: PermissionMode
}

interface UiState {
  drafts: Record<string, string>
  project: ProjectSelection | null
  selectedModel: string
  inspectorOpen: boolean
  inspectorTab: InspectorTab
  setDraft: (key: string, value: string) => void
  clearDraft: (key: string) => void
  setProject: (project: ProjectSelection | null) => void
  setSelectedModel: (model: string) => void
  setInspectorOpen: (open: boolean) => void
  setInspectorTab: (tab: InspectorTab) => void
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      drafts: {},
      // Keep the selection across settings navigation, but never persist a
      // directory grant beyond the current desktop session.
      project: null,
      selectedModel: '',
      inspectorOpen: false,
      inspectorTab: 'context',
      setDraft: (key, value) => set((state) => ({ drafts: { ...state.drafts, [key]: value } })),
      clearDraft: (key) =>
        set((state) => {
          const drafts = { ...state.drafts }
          delete drafts[key]
          return { drafts }
        }),
      setProject: (project) => set({ project }),
      setSelectedModel: (selectedModel) => set({ selectedModel }),
      setInspectorOpen: (inspectorOpen) => set({ inspectorOpen }),
      setInspectorTab: (inspectorTab) => set({ inspectorTab }),
    }),
    {
      name: 'open-harness-workspace',
      version: 1,
      partialize: ({ drafts, selectedModel, inspectorOpen, inspectorTab }) => ({
        drafts,
        selectedModel,
        inspectorOpen,
        inspectorTab,
      }),
    },
  ),
)
