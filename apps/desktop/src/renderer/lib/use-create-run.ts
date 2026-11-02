import { useEffect, useRef } from 'react'
import { useIsMutating, useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import type { CreateRunInput } from '../../shared/contracts'
import { storeSnapshot } from './queries'
import { useUiStore } from '../state/ui-store'

const createRunMutationKey = ['create-run'] as const

export function useCreateRun() {
  const client = useQueryClient()
  const navigate = useNavigate()
  const mounted = useRef(false)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])
  const pending = useIsMutating({ mutationKey: createRunMutationKey })
  const mutation = useMutation({
    mutationKey: createRunMutationKey,
    mutationFn: (input: CreateRunInput) => window.harness.createRun(input),
    onSuccess: (run, input) => {
      if (input.execution_policy?.approval_mode === 'full_access')
        useUiStore.getState().resetFullAccessDraft()
      storeSnapshot(client, run)
      if (mounted.current) void navigate({ to: '/runs/$runId', params: { runId: run.id } })
    },
  })
  return { ...mutation, isPending: mutation.isPending || pending > 0 }
}
