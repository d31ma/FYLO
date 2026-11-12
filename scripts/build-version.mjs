import { readFile } from 'node:fs/promises'

/**
 * The version a FYLO build reports (`FYLO_BUILD_VERSION`).
 *
 * A release build reports the `VERSION` file. So does any build whose
 * `VERSION` is newer than every `v<version>` tag, as on a release branch, or
 * whose commit carries the tag `v<VERSION>`. Any other build is a
 * development build of code after the newest release, and reports
 * `<newest release>-develop.<short commit>`, so it never looks older than
 * the release it follows. Without git or tags it reports `VERSION`.
 */
export class BuildVersion {
    static RELEASE_KIND = 'release'
    static TAG_PATTERN = /^v(\d+\.\d+\.\d+(?:-\d+)?)$/
    static SHORT_COMMIT_LENGTH = 7
    static DEVELOP_LABEL = 'develop'

    /**
     * @param {string} buildKind
     * @param {string} commit
     * @returns {Promise<string>}
     */
    static async resolve(buildKind, commit) {
        const fileVersion = (await readFile('VERSION', 'utf8')).trim()
        const released = await BuildVersion.newestRelease()
        const isRelease = await BuildVersion.isRelease(buildKind, fileVersion, released)
        return isRelease
            ? fileVersion
            : `${released}-${BuildVersion.DEVELOP_LABEL}.${await BuildVersion.shortCommit(commit)}`
    }

    /**
     * @param {string} buildKind
     * @param {string} fileVersion
     * @param {string | undefined} released
     * @returns {Promise<boolean>}
     */
    static async isRelease(buildKind, fileVersion, released) {
        if (buildKind === BuildVersion.RELEASE_KIND || released === undefined) return true
        if (BuildVersion.compare(fileVersion, released) > 0) return true
        return (await BuildVersion.git(['tag', '--points-at', 'HEAD']))
            .split('\n')
            .includes(`v${fileVersion}`)
    }

    /** @returns {Promise<string | undefined>} */
    static async newestRelease() {
        const tags = await BuildVersion.git(['tag', '--list', 'v[0-9]*'])
        let newest
        for (const tag of tags.split('\n')) {
            newest = BuildVersion.newer(newest, BuildVersion.TAG_PATTERN.exec(tag.trim())?.[1])
        }
        return newest
    }

    /**
     * @param {string | undefined} current
     * @param {string | undefined} candidate
     * @returns {string | undefined}
     */
    static newer(current, candidate) {
        if (candidate === undefined || current === undefined) return candidate ?? current
        return BuildVersion.compare(candidate, current) > 0 ? candidate : current
    }

    /**
     * @param {string} commit
     * @returns {Promise<string>}
     */
    static async shortCommit(commit) {
        const full = /^[0-9a-f]{40}$/i.test(commit)
            ? commit
            : (await BuildVersion.git(['rev-parse', 'HEAD'])).trim()
        return full.slice(0, BuildVersion.SHORT_COMMIT_LENGTH) || 'unknown'
    }

    /**
     * CalVer `YY.WW.DD` with an optional same-day `-N`, compared as numbers.
     *
     * @param {string} left
     * @param {string} right
     * @returns {number}
     */
    static compare(left, right) {
        const a = left.split(/[.-]/).map(Number)
        const b = right.split(/[.-]/).map(Number)
        const length = Math.max(a.length, b.length)
        let index = 0
        while (index < length && (a[index] ?? 0) === (b[index] ?? 0)) index++
        return (a[index] ?? 0) - (b[index] ?? 0)
    }

    /**
     * @param {string[]} command
     * @returns {Promise<string>}
     */
    static async git(command) {
        try {
            const child = Bun.spawn(['git', ...command], { stdout: 'pipe', stderr: 'pipe' })
            const [stdout, exitCode] = await Promise.all([
                new Response(child.stdout).text(),
                child.exited
            ])
            return exitCode === 0 ? stdout : ''
        } catch {
            return ''
        }
    }
}
