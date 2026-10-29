// Runs batches through a fresh `fylo-browser.wasm` instance over memory.
//
// Fresh, because a stack overflow unwinds the module mid-call and leaves it
// unusable; one Worker per run keeps a failure from hiding the next result.
// Batches cross as NDJSON text and answers come back the same way: a deep
// document passed as an object would recurse through the page's own
// (structured clone and Playwright's serializer) on the main thread instead.
// A request may name an earlier answer: the string "<result:k>" becomes the
// result of batch k's first frame, so a batch can use an id put before it.

import { createHostImports, exec } from '/src/browser/host-vfs.mjs'
import { createMemoryBackend } from './memory-backend.mjs'

self.onmessage = async (event) => {
    const { moduleUrl, batches } = event.data
    const answers = []
    try {
        let instance
        const imports = createHostImports({
            memory: () => instance.exports.memory,
            backend: createMemoryBackend()
        })
        instance = (await WebAssembly.instantiateStreaming(fetch(moduleUrl), imports)).instance
        const firstResult = (k) => JSON.parse(answers[k].split('\n')[0]).result
        for (const batch of batches) {
            const ndjson = batch.replace(/"<result:(\d+)>"/g, (_, k) =>
                JSON.stringify(firstResult(Number(k)) ?? null)
            )
            answers.push(exec(instance, ndjson))
        }
        self.postMessage({ answers })
    } catch (error) {
        self.postMessage({ answers, error: `${error?.name}: ${error?.message}` })
    }
}
