import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import type { ExtensionConfig } from '../../shared/extensions'

export const extensionSettingsOptions = queryOptions({
  queryKey: ['extension-settings'],
  queryFn: () => window.harness.extensionSettings(),
  staleTime: 30_000,
  retry: 1,
})

export function useSaveExtensions() {
  const client = useQueryClient()
  return useMutation({
    scope: { id: 'extension-config' },
    mutationFn: async (update: (config: ExtensionConfig) => ExtensionConfig) => {
      // Re-read persisted configuration so project skill toggles do not replace
      // a recently edited server list with an older settings-page snapshot.
      const current = await window.harness.extensionSettings()
      return window.harness.saveExtensions(update(current.config))
    },
    onSuccess: (settings) => {
      client.setQueryData(extensionSettingsOptions.queryKey, settings)
      void client.invalidateQueries({ queryKey: ['run-extensions'] })
    },
  })
}

export function withSkillEnabled(config: ExtensionConfig, id: string, enabled: boolean) {
  const disabled = new Set(config.disabled_skill_ids)
  if (enabled) disabled.delete(id)
  else disabled.add(id)
  return { ...config, disabled_skill_ids: [...disabled].sort() }
}
