import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export type InspectorTab = 'context' | 'timeline'

interface UiState {
  drafts: Record<string, string>
  selectedModel: string
  inspectorOpen: boolean
  inspectorTab: InspectorTab
  setDraft: (key: string, value: string) => void
  clearDraft: (key: string) => void
  setSelectedModel: (model: string) => void
  setInspectorOpen: (open: boolean) => void
  setInspectorTab: (tab: InspectorTab) => void
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      drafts: {},
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
