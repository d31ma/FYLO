// The crypto bridge Worker (ADR 0021, decision P5-5).
//
// The engine's Worker starts this one when a database opens as a
// `webcrypto:<name>` identity. It loads the identity's two non-extractable
// CryptoKeys from the origin's IndexedDB, reports `{ ready, publicKey }` (or
// `{ error, code }` when the origin holds no such identity), then answers the
// engine's calls through the shared buffer until it is terminated.
//
// Protocol: post `{ buffer, name }` once.

import {
    identityOperations,
    loadWebCryptoIdentity,
    serveIdentityBridge
} from './identity-bridge.mjs'

self.onmessage = async (event) => {
    const { buffer, name } = event.data ?? {}
    try {
        const identity = await loadWebCryptoIdentity(name)
        if (!identity) {
            self.postMessage({
                error: `this origin holds no identity webcrypto:${name}`,
                code: 'ENOIDENTITY'
            })
            return
        }
        self.postMessage({ ready: true, publicKey: identity.publicKey })
        await serveIdentityBridge(buffer, identityOperations(identity))
    } catch (error) {
        self.postMessage({
            error: error instanceof Error ? error.message : String(error),
            code: error?.code ?? 'EIDENTITY'
        })
    }
}
