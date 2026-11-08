// Runs batches through the engine in a Worker, over memory rather than OPFS.
//
// One Worker per call: `run(batches)` takes each batch as NDJSON text and
// resolves with each answer as NDJSON text, or with the error that stopped the
// Worker part way.

globalThis.run = (batches, moduleUrl = '/dist-web/fylo-browser.wasm') =>
    new Promise((resolve) => {
        const worker = new Worker('./engine-stack-worker.mjs', { type: 'module' })
        worker.onmessage = (event) => {
            worker.terminate()
            resolve(event.data)
        }
        worker.onerror = (event) => {
            worker.terminate()
            resolve({ answers: [], error: event.message ?? 'worker failed' })
        }
        worker.postMessage({ moduleUrl, batches })
    })
