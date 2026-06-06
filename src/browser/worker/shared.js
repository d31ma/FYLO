import { FyloWorkerRuntime, handleWorkerMessage } from './runtime.js'

const runtime = new FyloWorkerRuntime()
const sharedScope =
    /** @type {{ onconnect: ((event: MessageEvent & { ports: MessagePort[] }) => void) | null }} */ (
        /** @type {unknown} */ (globalThis)
    )

/** @param {MessageEvent & { ports: MessagePort[] }} event */
sharedScope.onconnect = (event) => {
    const port = event.ports[0]
    /** @param {MessageEvent} message */
    port.onmessage = (message) => {
        void handleWorkerMessage(runtime, port, message.data)
    }
    port.start()
}
