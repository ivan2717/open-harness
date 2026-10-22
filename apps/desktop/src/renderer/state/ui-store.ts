import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { PermissionMode, TextFileReference } from '../../shared/contracts'

export type InspectorTab = 'context' | 'timeline'

export interface ProjectSelection {
  path: string
  permission: PermissionMode
}

export interface DraftFileReference extends TextFileReference {
  id: string
}

interface UiState {
  drafts: Record<string, string>
  draftFiles: Record<string, DraftFileReference[]>
  fileImports: Record<string, boolean>
  project: ProjectSelection | null
  selectedModel: string
  inspectorOpen: boolean
  inspectorTab: InspectorTab
  setDraft: (key: string, value: string) => void
  clearDraft: (key: string) => void
  setDraftFiles: (key: string, files: DraftFileReference[]) => void
  removeDraftFiles: (key: string, ids: string[]) => void
  setFileImporting: (key: string, pending: boolean) => void
  setProject: (project: ProjectSelection | null) => void
  setSelectedModel: (model: string) => void
  setInspectorOpen: (open: boolean) => void
  setInspectorTab: (tab: InspectorTab) => void
}

export const useUiStore = create<UiState>()(
  persist(
    (set) => ({
      drafts: {},
      // File snapshots survive navigation, but unsent file contents are not
      // copied into localStorage. Sent snapshots belong to the durable run log.
      draftFiles: {},
      fileImports: {},
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
      setDraftFiles: (key, files) =>
        set((state) => ({ draftFiles: { ...state.draftFiles, [key]: files } })),
      removeDraftFiles: (key, ids) =>
        set((state) => {
          const draftFiles = { ...state.draftFiles }
          const remaining = (draftFiles[key] ?? []).filter((file) => !ids.includes(file.id))
          if (remaining.length) draftFiles[key] = remaining
          else delete draftFiles[key]
          return { draftFiles }
        }),
      setFileImporting: (key, pending) =>
        set((state) => {
          const fileImports = { ...state.fileImports }
          if (pending) fileImports[key] = true
          else delete fileImports[key]
          return { fileImports }
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
