import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Dev: proxy API + live updates to `wrangler dev` for the API Worker.
const api = process.env.CAIRN_API ?? 'http://127.0.0.1:8787';

export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: {
      '/api': api,
      '/healthz': api,
      '/hub': { target: api, ws: true },
    },
  },
});
