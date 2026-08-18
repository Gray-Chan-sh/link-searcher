import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    exclude: [
      'node_modules/**',
      '.opencode/**',
      'src/utils/__tests__/scopeParser.test.ts',
      'src/utils/__tests__/translateErr.test.ts',
    ],
  },
})