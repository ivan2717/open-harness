import type { AppConfig } from '../../shared/contracts'

export function taskModels(config: AppConfig, selectedId: string) {
  const models = config.models.filter((model) => model.id !== 'local-demo' && model.available)
  // Old preferences and the runtime's legacy default may still name the demo.
  // Only an available real model can become the default for a new task.
  const selected =
    models.find((model) => model.id === selectedId) ??
    models.find((model) => model.id === config.default_model) ??
    models[0]
  return { models, selected }
}
