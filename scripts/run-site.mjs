import { spawn } from 'node:child_process'
import { SiteArtifacts } from './site-artifacts.mjs'

class SiteCommand {
    static async run() {
        const [command, ...arguments_] = process.argv.slice(2)
        if (!['bundle', 'preview'].includes(command)) {
            throw new Error('Usage: run-site.mjs <bundle|preview> [options]')
        }
        const output = arguments_.some(
            (value) => value === '--out-dir' || value.startsWith('--out-dir=')
        )
            ? []
            : ['--out-dir', SiteArtifacts.web()]
        const child = spawn(process.env.TAC_BIN ?? 'ty', [command, ...output, ...arguments_], {
            stdio: 'inherit',
            env: process.env
        })
        for (const signal of ['SIGINT', 'SIGTERM']) {
            process.on(signal, () => child.kill(signal))
        }
        child.once('error', (error) => {
            throw error
        })
        child.once('exit', (code) => process.exit(code ?? 1))
    }
}

await SiteCommand.run()
