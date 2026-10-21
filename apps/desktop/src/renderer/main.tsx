import React from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Link,
  RouterProvider,
} from '@tanstack/react-router'
import { AppShell } from './components/AppShell'
import { RunPage, WelcomePage } from './pages/WorkspacePage'
import { SettingsPage } from './pages/SettingsPage'
import { errorMessage } from './lib/format'
import './styles.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { retry: 1, refetchOnWindowFocus: true, staleTime: 5_000 },
    mutations: { retry: false },
  },
})
const rootRoute = createRootRoute({
  component: AppShell,
  notFoundComponent: () => (
    <div className="empty-feedback">
      <h2>这个页面不存在</h2>
      <Link to="/" className="button button-secondary">
        返回工作区
      </Link>
    </div>
  ),
  errorComponent: ({ error, reset }) => (
    <div className="empty-feedback">
      <h2>界面遇到了问题</h2>
      <p>{errorMessage(error)}</p>
      <button className="button button-secondary" onClick={reset}>
        重新加载
      </button>
    </div>
  ),
})
const welcomeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: WelcomePage,
})
const runRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/runs/$runId',
  component: RunRoute,
})
const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  component: SettingsPage,
})
function RunRoute() {
  const { runId } = runRoute.useParams()
  return <RunPage key={runId} runId={runId} />
}
const router = createRouter({
  routeTree: rootRoute.addChildren([welcomeRoute, runRoute, settingsRoute]),
  history: createHashHistory(),
})
declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </React.StrictMode>,
)
