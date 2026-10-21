import { useEffect, useState } from 'react'
import { queryOptions, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import type { RunDetail, RunSummary } from '../../shared/contracts'
import { errorMessage } from './format'
import { latestSnapshot, mergeRunLists, updateRunList } from './run-snapshot'

export const configOptions = queryOptions({
  queryKey: ['config'],
  queryFn: () => window.harness.config(),
  staleTime: 60_000,
})

export const runsOptions = queryOptions({
  queryKey: ['runs'],
  queryFn: () => window.harness.listRuns(),
  refetchInterval: 15_000,
  structuralSharing: (old, incoming) =>
    mergeRunLists(old as RunSummary[] | undefined, incoming as RunSummary[]),
})

export function storeSnapshot(client: QueryClient, snapshot: RunDetail): void {
  const current = client.getQueryData<RunDetail>(['run', snapshot.id])
  const accepted = latestSnapshot(current, snapshot)
  client.setQueryData(['run', snapshot.id], accepted)
  client.setQueryData(runsOptions.queryKey, (runs) => updateRunList(runs, accepted))
}

export function useRun(id: string) {
  const client = useQueryClient()
  const [streamError, setStreamError] = useState<string | null>(null)
  const [connectionEpoch, setConnectionEpoch] = useState(0)
  const query = useQuery({
    queryKey: ['run', id],
    queryFn: () => window.harness.getRun(id),
    structuralSharing: (old, incoming) =>
      latestSnapshot(old as RunDetail | undefined, incoming as RunDetail),
    refetchInterval: streamError ? 5_000 : false,
  })

  useEffect(() => {
    let active = true
    setStreamError(null)
    let unsubscribe: (() => void) | undefined
    try {
      unsubscribe = window.harness.watchRun(
        id,
        (snapshot) => {
          if (!active || snapshot.id !== id) return
          setStreamError(null)
          storeSnapshot(client, snapshot)
        },
        (message) => {
          if (active) setStreamError(message)
        },
      )
    } catch (error) {
      setStreamError(errorMessage(error))
    }
    return () => {
      active = false
      unsubscribe?.()
    }
  }, [client, id, connectionEpoch])

  return {
    ...query,
    streamError,
    reconnect: () => {
      setConnectionEpoch((value) => value + 1)
      void query.refetch()
    },
  }
}
