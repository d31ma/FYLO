import { readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'

const enabled = process.env.FYLO_EXPLORER_DEDICATED_ORIGIN === '1'
if (enabled) {
    console.log('Explorer bundle retained for an explicit dedicated-origin build')
    process.exit(0)
}

const output = path.join(import.meta.dir, '..', 'dist', 'web')
const generatedExplorerPaths = [
    'explorer',
    'pages/explorer',
    'components/explorer',
    'shared/assets/explorer.css',
    'shared/assets/fylo-web.mjs',
    'shared/assets/highlight-theme.css',
    'shared/assets/highlight.min.js',
    'shared/assets/duvay/duvay-wc.min.js'
]

await Promise.all(
    generatedExplorerPaths.map((relativePath) =>
        rm(path.join(output, relativePath), { recursive: true, force: true })
    )
)

const rendererPath = path.join(output, 'spa-renderer.js')
let renderer = await readFile(rendererPath, 'utf8')
let manifestsPruned = 0
renderer = renderer.replace(/'(\{(?:\\.|[^'\\])*\})'/g, (literal, serialized) => {
    let manifest
    try {
        manifest = JSON.parse(serialized)
    } catch {
        return literal
    }
    if (!manifest || Array.isArray(manifest) || !Object.hasOwn(manifest, '/explorer')) {
        return literal
    }
    delete manifest['/explorer']
    manifestsPruned += 1
    return `'${JSON.stringify(manifest)}'`
})
if (
    manifestsPruned === 0 ||
    renderer.includes('"/explorer"') ||
    renderer.includes('/pages/explorer/')
) {
    throw new Error('Could not remove Explorer from the generated route manifest')
}
await writeFile(rendererPath, renderer)
console.log('Explorer removed from shared marketing build')
