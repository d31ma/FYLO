import { test as base, expect } from '@playwright/test'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

export { expect }

export const test = base.extend({
    context: async ({ browserName, context, playwright, baseURL, headless }, use) => {
        if (browserName !== 'webkit') {
            await use(context)
            return
        }
        const profile = await mkdtemp(join(tmpdir(), 'fylo-webkit-e2e-'))
        let persistentContext
        try {
            persistentContext = await playwright.webkit.launchPersistentContext(profile, {
                baseURL,
                headless
            })
            await use(persistentContext)
        } finally {
            try {
                await persistentContext?.close()
            } finally {
                await rm(profile, { recursive: true, force: true })
            }
        }
    }
})
