import { expect, test } from 'bun:test'

test('package entry exposes Fylo on globalThis for preloaded runtimes', async () => {
    const globalScope = /** @type {typeof globalThis & { Fylo?: unknown }} */ (globalThis)
    const previous = globalScope.Fylo
    delete globalScope.Fylo

    try {
        const module = await import(`../../src/index.js?global=${Date.now()}`)
        expect(globalScope.Fylo).toBe(module.default)
    } finally {
        if (previous === undefined) delete globalScope.Fylo
        else globalScope.Fylo = previous
    }
})
