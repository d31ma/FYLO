import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export class SiteArtifacts {
    static web(project = process.cwd()) {
        const website = fileURLToPath(new URL('../website/', import.meta.url))
        if (process.env.ANNEX_DEV_UNIT && resolve(project) === resolve(website)) {
            return resolve(process.env.ANNEX_DEV_UNIT, 'website-output', 'web')
        }
        return resolve(project, 'dist', 'web')
    }
}
