import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The viewer is emitted into the `codegraph-ui` crate, which embeds every file
// under it into the binary at compile time (its build.rs). The output is
// committed, so `cargo build` and `cargo install --git` never need Node; CI
// rebuilds it and fails when the committed copy differs.
//
// `fileURLToPath` (not a bare relative string) keeps this a native path on
// Windows, where Rollup resolves outDir against the platform separator.
const outDir = fileURLToPath(new URL('../crates/codegraph-ui/viewer', import.meta.url));

export default defineConfig(({ command }) => {
  // `vite build` does NOT override an ambient NODE_ENV, and Svelte compiles in
  // dev mode when it sees one — a shell (or a CI runner) with
  // NODE_ENV=development silently ships a viewer carrying Svelte's dev-only
  // runtime checks: ~13 kB larger, slower, and warning in the user's console.
  // A release artifact must not depend on the machine that built it.
  if (command === 'build') process.env.NODE_ENV = 'production';

  return {
    plugins: [svelte()],
    // Relative asset URLs: the CLI serves this at '/', but a relative base also
    // survives being opened from the filesystem or mounted under a sub-path.
    base: './',
    build: {
      // Scoped to the crate's viewer/ directory, which holds nothing but this
      // build's output — `emptyOutDir` must never be allowed to widen past it.
      outDir,
      emptyOutDir: true,
      target: 'es2022',
      // A localhost reader has the sources on disk already; sourcemaps would
      // double the bundle in every platform archive for no one's benefit.
      sourcemap: false,
      chunkSizeWarningLimit: 1024,
    },
    server: {
      host: '127.0.0.1',
      port: 5174,
    },
  };
});
