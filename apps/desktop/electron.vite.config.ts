import { resolve } from 'node:path'
import { defineConfig } from 'electron-vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import type { Plugin } from 'vite'

function contentSecurityPolicy(): Plugin {
  let development = false
  return {
    name: 'desktop-content-security-policy',
    configResolved: (config) => {
      development = config.command === 'serve'
    },
    transformIndexHtml: () => [
      {
        tag: 'meta',
        attrs: {
          'http-equiv': 'Content-Security-Policy',
          content: [
            "default-src 'self'",
            `script-src 'self'${development ? " 'unsafe-inline'" : ''}`,
            "style-src 'self' 'unsafe-inline'",
            "img-src 'self' data: blob: openharness-resource: https: http:",
            'media-src openharness-resource: https: http:',
            "font-src 'self' data:",
            development
              ? "connect-src 'self' ws://localhost:* ws://127.0.0.1:* http://localhost:* http://127.0.0.1:*"
              : "connect-src 'none'",
            "object-src 'none'",
            "frame-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
          ].join('; '),
        },
        injectTo: 'head-prepend',
      },
    ],
  }
}

export default defineConfig({
  main: { build: { rollupOptions: { input: resolve(__dirname, 'src/main/index.ts') } } },
  preload: {
    build: {
      rollupOptions: {
        input: resolve(__dirname, 'src/preload/index.ts'),
        output: { format: 'cjs', entryFileNames: 'index.cjs' },
      },
    },
  },
  renderer: {
    root: resolve(__dirname, 'src/renderer'),
    plugins: [react(), tailwindcss(), contentSecurityPolicy()],
    build: { rollupOptions: { input: resolve(__dirname, 'src/renderer/index.html') } },
  },
})
