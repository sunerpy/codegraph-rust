import { svelte, vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

/**
 * Two projects, one command (`npm test`), as upstream's `vitest.workspace.mts`:
 *
 * - `models` runs the suites that import `src/lib` models in plain Node;
 * - `package` mounts components against a mock adapter, which needs the Svelte
 *   plugin, jsdom and `resolve.conditions: ['browser']` (so `svelte` resolves to
 *   its client build — `mount()` throws on the server build). Applied globally,
 *   the browser condition would leak into the model suites, hence the split.
 */
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'models',
          include: ['tests/**/*.test.ts'],
          exclude: ['**/node_modules/**', 'tests/ui-package.test.ts'],
          environment: 'node',
        },
      },
      {
        plugins: [svelte({ preprocess: vitePreprocess() })],
        resolve: { conditions: ['browser'] },
        test: {
          name: 'package',
          globals: true,
          include: ['tests/ui-package.test.ts'],
          environment: 'jsdom',
          server: {
            deps: {
              // `@xyflow/svelte` ships uncompiled `.svelte` files, so it has to go
              // through the plugin above rather than be externalised to Node.
              inline: [/@xyflow\/svelte/],
            },
          },
        },
      },
    ],
  },
});
