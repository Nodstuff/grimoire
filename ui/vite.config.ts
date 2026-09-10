import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// GRIMOIRE_API=http://127.0.0.1:7519 npx vite → develop against a scratch
// daemon instead of the production one on 7425.
const target = process.env.GRIMOIRE_API ?? 'http://127.0.0.1:7425'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': target,
      '/admin': target,
    },
  },
})
