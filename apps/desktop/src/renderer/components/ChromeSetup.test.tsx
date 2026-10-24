import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { afterEach, expect, it, vi } from 'vitest'
import { ChromeSetup } from './ChromeSetup'

afterEach(() => vi.unstubAllGlobals())

it.each([
  ['win32', 'Ctrl+L', '⌘⇧G'],
  ['darwin', '⌘⇧G', 'Ctrl+L'],
])(
  'uses the %s file picker instructions for Chrome extension setup',
  (platform, expected, absent) => {
    vi.stubGlobal('window', { harness: { platform } })
    const client = new QueryClient()
    try {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={client}>
          <ChromeSetup
            connection={{
              configured: true,
              connected: false,
              storeUrl: null,
              extensionPath: '/extension',
            }}
          />
        </QueryClientProvider>,
      )
      expect(html).toContain(expected)
      expect(html).not.toContain(absent)
    } finally {
      client.clear()
    }
  },
)
