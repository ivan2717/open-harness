import type { CreateRunInput } from '../shared/contracts'

export const channels = {
  invoke: 'openharness:invoke',
  watchStart: 'openharness:watch:start',
  watchStop: 'openharness:watch:stop',
  watchEvent: 'openharness:watch:event',
} as const

export type BridgeRequest =
  | { operation: 'config' | 'listRuns' }
  | { operation: 'getRun' | 'cancelRun'; id: string }
  | { operation: 'createRun'; input: CreateRunInput }
  | { operation: 'submitInput'; id: string; message: string }

export interface ApiRequest {
  path: string
  method: 'GET' | 'POST'
  body?: CreateRunInput | { message: string }
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('Invalid request')
  return value as Record<string, unknown>
}

export function identifier(value: unknown): string {
  if (typeof value !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(value))
    throw new Error('Invalid run identifier')
  return value
}

function boundedText(value: unknown, maximum: number, field: string): string {
  if (typeof value !== 'string' || !value.trim() || value.length > maximum)
    throw new Error(`Invalid ${field}`)
  return value
}

export function apiRequest(value: unknown): ApiRequest {
  const request = object(value)
  switch (request.operation) {
    case 'config':
      return { path: '/api/config', method: 'GET' }
    case 'listRuns':
      return { path: '/api/runs', method: 'GET' }
    case 'getRun':
      return { path: `/api/runs/${identifier(request.id)}`, method: 'GET' }
    case 'cancelRun':
      return { path: `/api/runs/${identifier(request.id)}/cancel`, method: 'POST' }
    case 'createRun': {
      const input = object(request.input)
      return {
        path: '/api/runs',
        method: 'POST',
        body: {
          prompt: boundedText(input.prompt, 64_000, 'prompt'),
          model_id: boundedText(input.model_id, 256, 'model'),
        },
      }
    }
    case 'submitInput':
      return {
        path: `/api/runs/${identifier(request.id)}/input`,
        method: 'POST',
        body: { message: boundedText(request.message, 64_000, 'message') },
      }
    default:
      throw new Error('Unsupported desktop operation')
  }
}

export function watchRequest(value: unknown): { id: string; token: string } {
  const request = object(value)
  return { id: identifier(request.id), token: identifier(request.token) }
}
