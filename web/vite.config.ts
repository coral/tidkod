import { defineConfig } from 'vite';

// Cross-origin isolation raises performance.now() resolution from a coarsened ~100 µs to
// ~5 µs in Chromium. Probe and presentation timestamps use that timer directly. Production
// hosting must send the same headers; crossOriginIsolated reports whether it took effect.
const isolation = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

export default defineConfig({
  server: { headers: isolation },
  preview: { headers: isolation },
});
