import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import type { ExtensionConfig, PluginSourceKind } from '../../shared/extensions'
import { errorMessage } from './format'

export const extensionSettingsOptions = queryOptions({
  queryKey: ['extension-settings'],
  queryFn: () => window.harness.extensionSettings(),
  staleTime: 30_000,
  retry: 1,
})

export function useBundledPluginCatalog() {
  const catalog = useQuery({
    queryKey: ['bundled-plugins'],
    queryFn: () => window.harness.bundledPlugins(),
    staleTime: 30_000,
    refetchOnWindowFocus: 'always',
    retry: 1,
  })
  const { refetch } = catalog
  useEffect(() => {
    // Returning from macOS System Settings need not change visibilityState.
    const refresh = () => void refetch()
    window.addEventListener('focus', refresh)
    return () => window.removeEventListener('focus', refresh)
  }, [refetch])
  return catalog
}

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
      void client.invalidateQueries({ queryKey: ['run-memory'] })
      void client.invalidateQueries({ queryKey: ['skill-catalog'] })
      void client.invalidateQueries({ queryKey: ['skill-picker'] })
    },
  })
}

export function withSkillEnabled(config: ExtensionConfig, id: string, enabled: boolean) {
  const disabled = new Set(config.disabled_skill_ids)
  if (enabled) disabled.delete(id)
  else disabled.add(id)
  return { ...config, disabled_skill_ids: [...disabled].sort() }
}

type PluginAction =
  | { type: 'install'; id?: string; sourceKind: PluginSourceKind }
  | { type: 'enabled'; id: string; enabled: boolean }
  | { type: 'uninstall'; id: string }

export function usePluginActions() {
  const client = useQueryClient()
  return useMutation({
    scope: { id: 'extension-config' },
    mutationFn: (action: PluginAction) => {
      switch (action.type) {
        case 'install':
          return window.harness.installPlugin(action.id, action.sourceKind)
        case 'enabled':
          return window.harness.setPluginEnabled(action.id, action.enabled)
        case 'uninstall':
          return window.harness.uninstallPlugin(action.id)
      }
    },
    onSuccess: (settings) => {
      if (!settings) return
      client.setQueryData(extensionSettingsOptions.queryKey, settings)
      void client.invalidateQueries({ queryKey: ['run-extensions'] })
      void client.invalidateQueries({ queryKey: ['run-memory'] })
      void client.invalidateQueries({ queryKey: ['skill-catalog'] })
      void client.invalidateQueries({ queryKey: ['skill-picker'] })
    },
  })
}

export function pluginErrorMessage(error: unknown): string {
  const message = errorMessage(error).replace(
    /^Error invoking remote method '[^']+': (?:Error: )?/,
    '',
  )
  if (message === 'updated plugin manifest name must match the installed identity')
    return '请选择同一插件的新版本，当前插件保持不变。'
  if (message === 'plugin already installed; use update to replace it')
    return '此插件已安装，请使用它的「更新」入口。'
  if (message === 'unknown plugin' || message === 'cannot update an unknown plugin')
    return '这个插件已不存在，请刷新列表。'
  return message
}
