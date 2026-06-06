import { FyloWorkerRuntime, handleWorkerMessage } from './runtime.js'

const runtime = new FyloWorkerRuntime()
const workerScope =
    /** @type {{ onmessage: ((message: MessageEvent) => void) | null, postMessage: (message: any) => void }} */ (
        /** @type {unknown} */ (globalThis)
    )

/** @param {MessageEvent} message */
workerScope.onmessage = (message) => {
    void handleWorkerMessage(runtime, workerScope, message.data)
}
