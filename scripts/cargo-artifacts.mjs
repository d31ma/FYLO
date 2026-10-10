import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export class CargoArtifacts {
    static report(value) {
        if (process.env.CARGO_TARGET_DIR && value.startsWith('target/')) {
            return CargoArtifacts.path(value.slice(7))
        }
        return value
    }

    static path(...parts) {
        const directory = process.env.CARGO_TARGET_DIR
            ? resolve(process.env.CARGO_TARGET_DIR)
            : fileURLToPath(new URL('../target/', import.meta.url))
        return resolve(directory, ...parts)
    }
}
