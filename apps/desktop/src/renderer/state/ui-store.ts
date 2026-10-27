import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { PermissionMode, TextFileReference } from '../../shared/contracts'

export type InspectorTab = 'context' | 'timeline' | 'memory'

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
  projectPaths: string[]
  selectedModel: string
  draftReasoningEfforts: Record<string, string>
  inspectorOpen: boolean
  inspectorTab: InspectorTab
  setDraft: (key: string, value: string) => void
  clearDraft: (key: string) => void
  setDraftFiles: (key: string, files: DraftFileReference[]) => void
  removeDraftFiles: (key: string, ids: string[]) => void
  setFileImporting: (key: string, pending: boolean) => void
  setProject: (project: ProjectSelection | null) => void
  setSelectedModel: (model: string) => void
  setDraftReasoningEffort: (model: string, effort: string | null) => void
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
      // Keep the draft selection across navigation; the main process validates
      // saved projects when they are reopened after an app restart.
      project: null,
      // Remember sidebar entries, not filesystem grants or permission modes.
      projectPaths: [],
      selectedModel: '',
      // A new task can choose an override without changing the saved model.
      // These draft choices survive navigation, but are not persisted to disk.
      draftReasoningEfforts: {},
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
      setProject: (project) =>
        set((state) => ({
          project,
          projectPaths:
            project && !state.projectPaths.includes(project.path)
              ? [...state.projectPaths, project.path]
              : state.projectPaths,
        })),
      setSelectedModel: (selectedModel) => set({ selectedModel }),
      setDraftReasoningEffort: (model, effort) =>
        set((state) => {
          const draftReasoningEfforts = { ...state.draftReasoningEfforts }
          if (effort === null) delete draftReasoningEfforts[model]
          else draftReasoningEfforts[model] = effort
          return { draftReasoningEfforts }
        }),
      setInspectorOpen: (inspectorOpen) => set({ inspectorOpen }),
      setInspectorTab: (inspectorTab) => set({ inspectorTab }),
    }),
    {
      name: 'open-harness-workspace',
      version: 1,
      partialize: ({ drafts, projectPaths, selectedModel, inspectorOpen, inspectorTab }) => ({
        drafts,
        projectPaths,
        selectedModel,
        inspectorOpen,
        inspectorTab,
      }),
    },
  ),
)
