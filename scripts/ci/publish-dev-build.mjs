import { spawn, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream, rmSync } from 'node:fs'
import {
    appendFile,
    chmod,
    mkdir,
    readFile,
    readdir,
    realpath,
    rename,
    rm,
    stat,
    writeFile
} from 'node:fs/promises'
import { homedir } from 'node:os'
import path from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'

/**
 * @typedef {{ name: string, file: string, vm: string, triple: string }} Platform
 * @typedef {{ runtimeVersion: string, commit: string, buildKind: string, buildTarget: string }} BuildIdentity
 * @typedef {{ sha256: string, rustc: string, target: string, glibc?: string, identity: BuildIdentity }} LaneReport
 * @typedef {{ file: string, sha256: string, bytes: number, rustc: string, rustTarget: string, version: string, buildTarget: string, glibc?: string }} PlatformEntry
 * @typedef {{ mode: string, commit: string, platform: string, out: string, logs: string, gateLog: string, ungated: boolean, contains: string[] }} Options
 * @typedef {{ gateLog: string, contains: string[] }} Provenance
 * @typedef {{ root: string, clone: string, worktree: string, target: string, colimaEnvironment: string, logRoot: string }} Settings
 * @typedef {{ cwd?: string, env?: Record<string, string | undefined>, log?: string, prefix?: string }} StreamOptions
 * @typedef {{ platform: string, exitCode: number }} LaneResult
 */

export class PublishDevBuild {
    /** @type {Platform[]} */
    static PLATFORMS = [
        { name: 'macos-arm64', file: 'fylo', vm: '', triple: 'aarch64-apple-darwin' },
        { name: 'linux-arm64', file: 'fylo', vm: 'colima', triple: 'aarch64-unknown-linux-gnu' },
        {
            name: 'windows-arm64',
            file: 'fylo.exe',
            vm: 'windows',
            triple: 'aarch64-pc-windows-msvc'
        }
    ]
    static MAC = 'macos-arm64'
    static LINUX = 'linux-arm64'
    static WINDOWS = 'windows-arm64'
    static BUILD_KIND = 'development-compiled'
    static MAX_GLIBC = '2.39'
    static COMMIT_PATTERN = /^[0-9a-f]{40}$/
    static HASH_PATTERN = /^[0-9a-f]{64}$/
    static PRODUCTION_VOLUMES = ['/Volumes/HAVEN', '/Volumes/ATLAS']
    static DEVELOP_REF = 'origin/develop'
    static COORDINATOR = 'fylo'
    static OWNER_PREFIX = 'fylo-publish-'
    static LOCK_NAME = '.publish.lock'
    static LOCK_OWNER = 'owner'
    static STAGING_PREFIX = '.staging-'
    static LANE_SUFFIX = '.lane.json'
    static MANIFEST = 'manifest.json'
    static CHECKSUM = 'fylo.sha256'
    static MANIFEST_SCHEMA = 1
    static READ_ONLY_FILE = 0o444
    static READ_ONLY_EXECUTABLE = 0o555
    static SHORT_COMMIT_LENGTH = 7
    static CONTAINER = 'fylo-publish-linux'
    static VOLUME = 'fylo-publish-linux-home'
    static IMAGE = 'tachyon-linux-ci'
    static CONTAINER_HOME = '/home/ci'
    static CONTAINER_SCRIPT = '/home/ci/publish-dev-build-linux.sh'
    static CONTAINER_OUT = '/home/ci/fylo-publish/out'
    static WINDOWS_HOST = 'windows-vm'
    static WINDOWS_SCRIPT = 'fylo-publish-dev-build.ps1'
    static WINDOWS_OUT = 'fylo-publish/out'
    static LANE_REPORT = 'lane.json'
    static SSH_FAILURE = 255
    static SSH_RETRY_MILLISECONDS = 30000
    static OPTION_FLAGS = ['--lane', '--sha', '--out', '--logs', '--gate-log', '--contains']
    static UNGATED_FLAG = '--ungated'
    static SIGNAL_EXIT = 143
    static USAGE =
        'Usage: bun scripts/ci/publish-dev-build.mjs <develop commit> (--gate-log <file> | --ungated)\n' +
        '           [--contains <commit>]...\n' +
        'Builds fylo for macos-arm64, linux-arm64 and windows-arm64 and publishes them to\n' +
        '$FYLO_DEV_BUILDS_ROOT/<sha> (default ~/Developer/AnvilTargets/fylo-builds/<sha>).'
    static SCRIPT = fileURLToPath(import.meta.url)
    static SCRIPT_DIRECTORY = path.dirname(fileURLToPath(import.meta.url))
    static HOME = homedir()

    /**
     * @param {string[]} argv
     * @returns {Promise<void>}
     */
    static async main(argv) {
        process.exitCode = await PublishDevBuild.#guarded(argv)
    }

    /**
     * @param {string[]} argv
     * @returns {Options}
     */
    static parse(argv) {
        /** @type {Options} */
        const options = {
            mode: 'publish',
            commit: '',
            platform: '',
            out: '',
            logs: '',
            gateLog: '',
            ungated: false,
            contains: []
        }
        PublishDevBuild.#readArguments(argv, options)
        PublishDevBuild.#requireOptions(options)
        return options
    }

    /** @returns {Settings} */
    static settings() {
        const anvil = path.join(PublishDevBuild.HOME, 'Developer', 'AnvilTargets')
        return {
            root: PublishDevBuild.#environment(
                'FYLO_DEV_BUILDS_ROOT',
                path.join(anvil, 'fylo-builds')
            ),
            clone: PublishDevBuild.#environment(
                'FYLO_PUBLISH_CLONE',
                path.join(anvil, 'git', 'FYLO')
            ),
            worktree: path.join(anvil, 'work', 'FYLO', 'publish-build'),
            target: path.join(anvil, 'targets', 'publish'),
            colimaEnvironment: path.join(
                PublishDevBuild.HOME,
                '.local/share/tachyon-ci/docker-linux/colima-env.zsh'
            ),
            logRoot: PublishDevBuild.#logRoot()
        }
    }

    /**
     * @param {string} directory
     * @returns {Promise<void>}
     */
    static async refuseProductionVolume(directory) {
        const real = await PublishDevBuild.#existingRealPath(directory)
        const production = PublishDevBuild.PRODUCTION_VOLUMES.some((volume) =>
            PublishDevBuild.#isInside(real, volume)
        )
        PublishDevBuild.#require(
            !production,
            `${real} is on HAVEN or ATLAS, which hold production data; publish builds to ANVIL`
        )
    }

    /**
     * @param {string} left
     * @param {string} right
     * @returns {number}
     */
    static compareVersions(left, right) {
        const a = left.split('.').map(Number)
        const b = right.split('.').map(Number)
        const length = Math.max(a.length, b.length)
        let index = 0
        while (index < length && (a[index] ?? 0) === (b[index] ?? 0)) index++
        return (a[index] ?? 0) - (b[index] ?? 0)
    }

    /**
     * @param {string} file
     * @returns {Promise<string>}
     */
    static async sha256(file) {
        const hash = createHash('sha256')
        for await (const chunk of createReadStream(file)) hash.update(chunk)
        return hash.digest('hex')
    }

    /**
     * @param {string} root
     * @returns {Promise<string>}
     */
    static async acquireLock(root) {
        const lock = path.join(root, PublishDevBuild.LOCK_NAME)
        await PublishDevBuild.#removeStaleLock(lock)
        await PublishDevBuild.#createLock(lock)
        return lock
    }

    /**
     * @param {string} lock
     * @returns {Promise<void>}
     */
    static async releaseLock(lock) {
        await rm(lock, { recursive: true, force: true })
    }

    /**
     * Checks one lane's files in the staging folder, writes `fylo.sha256`, makes the files
     * read-only and returns the platform's manifest entry.
     *
     * @param {string} staging
     * @param {Platform} platform
     * @param {string} commit
     * @returns {Promise<PlatformEntry>}
     */
    static async collect(staging, platform, commit) {
        const reportFile = path.join(staging, `${platform.name}${PublishDevBuild.LANE_SUFFIX}`)
        /** @type {LaneReport} */
        const report = JSON.parse(await readFile(reportFile, 'utf8'))
        const binary = path.join(staging, platform.name, platform.file)
        const digest = await PublishDevBuild.sha256(binary)
        PublishDevBuild.#checkReport(report, digest, platform, commit)
        await PublishDevBuild.#writeChecksum(binary, digest, platform.file)
        await rm(reportFile)
        return PublishDevBuild.#entry(report, digest, platform, (await stat(binary)).size)
    }

    /**
     * @param {string} commit
     * @param {PlatformEntry[]} entries
     * @param {Date} builtAt
     * @param {Provenance} provenance
     * @returns {Record<string, unknown>}
     */
    static manifest(commit, entries, builtAt, provenance) {
        PublishDevBuild.#requireOne(
            entries.map((entry) => entry.version),
            'versions'
        )
        PublishDevBuild.#requireOne(
            entries.map((entry) => entry.rustc),
            'Rust compilers'
        )
        return {
            schema: PublishDevBuild.MANIFEST_SCHEMA,
            sha: commit,
            contains: provenance.contains,
            gated: provenance.gateLog !== '',
            gateLog: provenance.gateLog || null,
            buildKind: PublishDevBuild.BUILD_KIND,
            version: entries[0].version,
            builtAt: builtAt.toISOString(),
            rustc: entries[0].rustc,
            rustTargets: entries.map((entry) => entry.rustTarget),
            platforms: Object.fromEntries(
                PublishDevBuild.PLATFORMS.map((platform, index) => [platform.name, entries[index]])
            )
        }
    }

    /**
     * Moves a finished staging folder to its final name in one rename, so a reader sees a
     * whole build or nothing. A finished folder is never replaced.
     *
     * @param {string} staging
     * @param {string} final
     * @returns {Promise<void>}
     */
    static async finish(staging, final) {
        await PublishDevBuild.#requireAbsent(final)
        await rename(staging, final)
    }

    /**
     * @param {string[]} argv
     * @returns {Promise<number>}
     */
    static async #guarded(argv) {
        try {
            return await PublishDevBuild.#dispatch(PublishDevBuild.parse(argv))
        } catch (error) {
            console.error(`publish-dev-build: ${PublishDevBuild.#message(error)}`)
            return 1
        }
    }

    /**
     * @param {Options} options
     * @returns {Promise<number>}
     */
    static async #dispatch(options) {
        if (options.mode === 'lane') return PublishDevBuild.#lane(options)
        return PublishDevBuild.#publish(options)
    }

    /**
     * @param {Options} options
     * @returns {Promise<number>}
     */
    static async #publish(options) {
        const settings = PublishDevBuild.settings()
        await mkdir(settings.root, { recursive: true })
        await PublishDevBuild.refuseProductionVolume(settings.root)
        const commit = PublishDevBuild.#resolveCommit(settings.clone, options.commit)
        const provenance = await PublishDevBuild.#provenance(settings.clone, commit, options)
        const final = path.join(settings.root, commit)
        if (await PublishDevBuild.#exists(final)) return PublishDevBuild.#alreadyPublished(final)
        const lock = await PublishDevBuild.acquireLock(settings.root)
        const staging = path.join(settings.root, `${PublishDevBuild.STAGING_PREFIX}${commit}`)
        PublishDevBuild.#cleanUpOnSignal([staging, lock])
        try {
            return await PublishDevBuild.#publishLocked(settings, commit, provenance, staging)
        } finally {
            await PublishDevBuild.releaseLock(lock)
        }
    }

    /**
     * @param {Settings} settings
     * @param {string} commit
     * @param {Provenance} provenance
     * @param {string} staging
     * @returns {Promise<number>}
     */
    static async #publishLocked(settings, commit, provenance, staging) {
        const final = path.join(settings.root, commit)
        if (await PublishDevBuild.#exists(final)) return PublishDevBuild.#alreadyPublished(final)
        await PublishDevBuild.#removeOldStaging(settings.root)
        const logs = PublishDevBuild.#logDirectory(settings.logRoot, commit)
        try {
            await PublishDevBuild.#build(settings, commit, provenance, staging, logs)
            await PublishDevBuild.finish(staging, final)
        } finally {
            await rm(staging, { recursive: true, force: true })
        }
        console.log(`publish-dev-build: published ${final}`)
        return 0
    }

    /**
     * @param {Settings} settings
     * @param {string} commit
     * @param {Provenance} provenance
     * @param {string} staging
     * @param {string} logs
     * @returns {Promise<void>}
     */
    static async #build(settings, commit, provenance, staging, logs) {
        await mkdir(logs, { recursive: true })
        console.log(`publish-dev-build: ${commit}, logs in ${logs}`)
        await PublishDevBuild.#prepareWorktree(settings, commit)
        await PublishDevBuild.#makePlatformFolders(staging)
        const results = await Promise.all(
            PublishDevBuild.PLATFORMS.map((platform) =>
                PublishDevBuild.#queueLane(platform, commit, staging, logs)
            )
        )
        PublishDevBuild.#requireLanesPassed(results, logs)
        const entries = []
        for (const platform of PublishDevBuild.PLATFORMS) {
            entries.push(await PublishDevBuild.collect(staging, platform, commit))
        }
        const manifest = PublishDevBuild.manifest(commit, entries, new Date(), provenance)
        await PublishDevBuild.#writeManifest(staging, manifest)
    }

    /**
     * @param {Platform} platform
     * @param {string} commit
     * @param {string} staging
     * @param {string} logs
     * @returns {Promise<LaneResult>}
     */
    static async #queueLane(platform, commit, staging, logs) {
        const command = [
            'mac-heavy',
            '--owner',
            `${PublishDevBuild.OWNER_PREFIX}${platform.name}`,
            ...PublishDevBuild.#vmFlags(platform),
            '--',
            process.execPath,
            PublishDevBuild.SCRIPT,
            '--lane',
            platform.name,
            '--sha',
            commit,
            '--out',
            path.join(staging, platform.name),
            '--logs',
            logs
        ]
        const exitCode = await PublishDevBuild.#stream(command, {
            env: { ...process.env, MAC_HEAVY_COORDINATOR: PublishDevBuild.COORDINATOR },
            log: path.join(logs, `${platform.name}.log`),
            prefix: `[${platform.name}] `
        })
        return { platform: platform.name, exitCode }
    }

    /**
     * @param {Options} options
     * @returns {Promise<number>}
     */
    static async #lane(options) {
        console.log(`=== lane ${options.platform} ${options.commit} ${new Date().toISOString()}`)
        const report = await PublishDevBuild.#runLane(options)
        const reportFile = `${options.out}${PublishDevBuild.LANE_SUFFIX}`
        await writeFile(reportFile, `${JSON.stringify(report, null, 4)}\n`)
        console.log(`=== lane ${options.platform} rc=0 sha256=${report.sha256}`)
        return 0
    }

    /**
     * @param {Options} options
     * @returns {Promise<LaneReport>}
     */
    static async #runLane(options) {
        if (options.platform === PublishDevBuild.LINUX) return PublishDevBuild.#linuxLane(options)
        if (options.platform === PublishDevBuild.WINDOWS) {
            return PublishDevBuild.#windowsLane(options)
        }
        return PublishDevBuild.#macLane(options)
    }

    /**
     * @param {Options} options
     * @returns {Promise<LaneReport>}
     */
    static async #macLane(options) {
        const settings = PublishDevBuild.settings()
        PublishDevBuild.#requireHead(settings.worktree, options.commit)
        const binary = path.join(options.out, 'fylo')
        await PublishDevBuild.#checkedStream(
            [process.execPath, './scripts/build-executable.mjs', '--outfile', binary],
            {
                cwd: settings.worktree,
                env: {
                    ...process.env,
                    CARGO_TARGET_DIR: settings.target,
                    FYLO_BUILD_COMMIT: options.commit,
                    FYLO_BUILD_KIND: PublishDevBuild.BUILD_KIND
                }
            }
        )
        return {
            sha256: await PublishDevBuild.sha256(binary),
            rustc: PublishDevBuild.#capture(['rustc', '--version'], settings.worktree).trim(),
            target: PublishDevBuild.#rustHost(settings.worktree),
            identity: JSON.parse(
                PublishDevBuild.#capture([binary, 'version', '--output', 'json'], settings.worktree)
            )
        }
    }

    /**
     * @param {Options} options
     * @returns {Promise<LaneReport>}
     */
    static async #linuxLane(options) {
        const settings = PublishDevBuild.settings()
        const colimaLog = path.join(options.logs, 'colima.log')
        const environment = PublishDevBuild.#dockerEnvironment(settings.colimaEnvironment)
        try {
            await PublishDevBuild.#colima(settings.colimaEnvironment, 'colima_start', colimaLog)
            await PublishDevBuild.#ensureContainer(environment)
            return await PublishDevBuild.#buildInContainer(options, environment)
        } finally {
            await PublishDevBuild.#stream(['docker', 'stop', PublishDevBuild.CONTAINER], {
                env: environment
            })
            await PublishDevBuild.#colima(settings.colimaEnvironment, 'colima_stop', colimaLog)
        }
    }

    /**
     * @param {Options} options
     * @param {Record<string, string | undefined>} environment
     * @returns {Promise<LaneReport>}
     */
    static async #buildInContainer(options, environment) {
        const container = PublishDevBuild.CONTAINER
        const script = path.join(PublishDevBuild.SCRIPT_DIRECTORY, 'publish-dev-build-linux.sh')
        const report = path.join(
            options.logs,
            `${PublishDevBuild.LINUX}-${PublishDevBuild.LANE_REPORT}`
        )
        const out = PublishDevBuild.CONTAINER_OUT
        await PublishDevBuild.#docker(environment, [
            'cp',
            script,
            `${container}:${PublishDevBuild.CONTAINER_SCRIPT}`
        ])
        await PublishDevBuild.#docker(environment, [
            'exec',
            '-u',
            'ci',
            '-w',
            PublishDevBuild.CONTAINER_HOME,
            container,
            'bash',
            PublishDevBuild.CONTAINER_SCRIPT,
            options.commit
        ])
        await PublishDevBuild.#docker(environment, [
            'cp',
            `${container}:${out}/fylo`,
            path.join(options.out, 'fylo')
        ])
        await PublishDevBuild.#docker(environment, [
            'cp',
            `${container}:${out}/${PublishDevBuild.LANE_REPORT}`,
            report
        ])
        return JSON.parse(await readFile(report, 'utf8'))
    }

    /**
     * @param {Record<string, string | undefined>} environment
     * @returns {Promise<void>}
     */
    static async #ensureContainer(environment) {
        const image = PublishDevBuild.#newestImage(environment)
        const current = PublishDevBuild.#containerImage(environment)
        if (current !== image) await PublishDevBuild.#createContainer(environment, image)
        await PublishDevBuild.#docker(environment, ['start', PublishDevBuild.CONTAINER])
        await PublishDevBuild.#docker(environment, [
            'exec',
            '-u',
            'root',
            PublishDevBuild.CONTAINER,
            'chown',
            'ci:ci',
            PublishDevBuild.CONTAINER_HOME
        ])
        console.log(`container ${PublishDevBuild.CONTAINER} from ${image}`)
    }

    /**
     * @param {Record<string, string | undefined>} environment
     * @param {string} image
     * @returns {Promise<void>}
     */
    static async #createContainer(environment, image) {
        await PublishDevBuild.#stream(['docker', 'rm', '-f', PublishDevBuild.CONTAINER], {
            env: environment
        })
        await PublishDevBuild.#docker(environment, [
            'run',
            '-d',
            '--init',
            '--name',
            PublishDevBuild.CONTAINER,
            '--platform',
            'linux/arm64',
            '--ulimit',
            'core=0',
            '-v',
            `${PublishDevBuild.VOLUME}:${PublishDevBuild.CONTAINER_HOME}`,
            image,
            'sleep',
            'infinity'
        ])
    }

    /**
     * @param {Record<string, string | undefined>} environment
     * @returns {string}
     */
    static #newestImage(environment) {
        const text = PublishDevBuild.#capture(
            [
                'docker',
                'images',
                PublishDevBuild.IMAGE,
                '--format',
                '{{.CreatedAt}}|{{.Repository}}:{{.Tag}}'
            ],
            PublishDevBuild.HOME,
            environment
        )
        const newest = text.split('\n').filter(Boolean).sort().reverse()[0]
        PublishDevBuild.#require(
            newest !== undefined,
            `no ${PublishDevBuild.IMAGE} image in Colima`
        )
        return newest.split('|')[1]
    }

    /**
     * @param {Record<string, string | undefined>} environment
     * @returns {string}
     */
    static #containerImage(environment) {
        const result = spawnSync(
            'docker',
            ['container', 'inspect', '--format', '{{.Config.Image}}', PublishDevBuild.CONTAINER],
            { encoding: 'utf8', env: environment }
        )
        return result.status === 0 ? result.stdout.trim() : ''
    }

    /**
     * @param {Options} options
     * @returns {Promise<LaneReport>}
     */
    static async #windowsLane(options) {
        const host = PublishDevBuild.WINDOWS_HOST
        const script = path.join(PublishDevBuild.SCRIPT_DIRECTORY, 'publish-dev-build-windows.ps1')
        const report = path.join(
            options.logs,
            `${PublishDevBuild.WINDOWS}-${PublishDevBuild.LANE_REPORT}`
        )
        const out = PublishDevBuild.WINDOWS_OUT
        try {
            await PublishDevBuild.#checkedStream(['testvm', 'profile', 'ci', 'windows'], {})
            await PublishDevBuild.#checkedStream(['testvm', 'start', 'windows'], {})
            await PublishDevBuild.#remote([
                'scp',
                '-q',
                script,
                `${host}:${PublishDevBuild.WINDOWS_SCRIPT}`
            ])
            await PublishDevBuild.#remote([
                'ssh',
                host,
                `powershell -NoProfile -ExecutionPolicy Bypass -File ${PublishDevBuild.WINDOWS_SCRIPT} -Sha ${options.commit}`
            ])
            await PublishDevBuild.#remote([
                'scp',
                '-q',
                `${host}:${out}/fylo.exe`,
                path.join(options.out, 'fylo.exe')
            ])
            await PublishDevBuild.#remote([
                'scp',
                '-q',
                `${host}:${out}/${PublishDevBuild.LANE_REPORT}`,
                report
            ])
            return JSON.parse(PublishDevBuild.#withoutByteOrderMark(await readFile(report, 'utf8')))
        } finally {
            await PublishDevBuild.#stream(['testvm', 'profile', 'idle', 'windows'], {})
            await PublishDevBuild.#stream(['testvm', 'stop', 'windows'], {})
        }
    }

    /**
     * Runs an ssh or scp command, once more after a pause when it fails with 255: the VM's
     * sshd sometimes drops one connection after a burst of short sessions.
     *
     * @param {string[]} command
     * @returns {Promise<void>}
     */
    static async #remote(command) {
        const exitCode = await PublishDevBuild.#stream(command, {})
        if (exitCode !== PublishDevBuild.SSH_FAILURE) {
            return PublishDevBuild.#require(
                exitCode === 0,
                `${command.join(' ')} failed (exit ${exitCode})`
            )
        }
        console.log(`=== ${command[0]} retry after rc=${exitCode}`)
        await Bun.sleep(PublishDevBuild.SSH_RETRY_MILLISECONDS)
        await PublishDevBuild.#checkedStream(command, {})
    }

    /**
     * @param {string} text
     * @returns {string}
     */
    static #withoutByteOrderMark(text) {
        return text.replace(/^﻿/, '')
    }

    /**
     * @param {string} colimaEnvironment
     * @returns {Record<string, string | undefined>}
     */
    static #dockerEnvironment(colimaEnvironment) {
        const host = PublishDevBuild.#capture(
            ['zsh', '-c', 'source "$1" && print -r -- "$DOCKER_HOST"', 'zsh', colimaEnvironment],
            PublishDevBuild.HOME
        ).trim()
        PublishDevBuild.#require(host !== '', `${colimaEnvironment} did not set DOCKER_HOST`)
        return { ...process.env, DOCKER_HOST: host }
    }

    /**
     * @param {string} colimaEnvironment
     * @param {string} action
     * @param {string} log
     * @returns {Promise<void>}
     */
    static async #colima(colimaEnvironment, action, log) {
        await PublishDevBuild.#checkedStream(
            ['zsh', '-c', `source "$1" && ${action} "$2"`, 'zsh', colimaEnvironment, log],
            {}
        )
    }

    /**
     * @param {Record<string, string | undefined>} environment
     * @param {string[]} args
     * @returns {Promise<void>}
     */
    static async #docker(environment, args) {
        await PublishDevBuild.#checkedStream(['docker', ...args], { env: environment })
    }

    /**
     * @param {Settings} settings
     * @param {string} commit
     * @returns {Promise<void>}
     */
    static async #prepareWorktree(settings, commit) {
        if (!(await PublishDevBuild.#exists(settings.worktree))) {
            PublishDevBuild.#git(settings.clone, [
                'worktree',
                'add',
                '--detach',
                settings.worktree,
                commit
            ])
            return
        }
        PublishDevBuild.#git(settings.worktree, ['checkout', '--force', '--detach', commit])
        PublishDevBuild.#git(settings.worktree, ['clean', '-fdx'])
    }

    /**
     * @param {string} clone
     * @param {string} input
     * @returns {string}
     */
    static #resolveCommit(clone, input) {
        PublishDevBuild.#git(clone, ['fetch', '--tags', 'origin'])
        const commit = PublishDevBuild.#git(clone, [
            'rev-parse',
            '--verify',
            `${input}^{commit}`
        ]).trim()
        const result = spawnSync(
            'git',
            ['-C', clone, 'merge-base', '--is-ancestor', commit, PublishDevBuild.DEVELOP_REF],
            { encoding: 'utf8' }
        )
        PublishDevBuild.#require(
            result.status === 0,
            `${commit} is not on ${PublishDevBuild.DEVELOP_REF}`
        )
        return commit
    }

    /**
     * @param {string} worktree
     * @param {string} commit
     * @returns {void}
     */
    static #requireHead(worktree, commit) {
        const head = PublishDevBuild.#git(worktree, ['rev-parse', 'HEAD']).trim()
        PublishDevBuild.#require(head === commit, `${worktree} is at ${head}, not ${commit}`)
    }

    /**
     * @param {string} directory
     * @param {string[]} args
     * @returns {string}
     */
    static #git(directory, args) {
        return PublishDevBuild.#capture(['git', '-C', directory, ...args], PublishDevBuild.HOME)
    }

    /**
     * @param {string[]} argv
     * @param {Options} options
     * @returns {void}
     */
    static #readArguments(argv, options) {
        for (let index = 0; index < argv.length; index++) {
            index = PublishDevBuild.#readArgument(argv, index, options)
        }
    }

    /**
     * @param {string[]} argv
     * @param {number} index
     * @param {Options} options
     * @returns {number}
     */
    static #readArgument(argv, index, options) {
        const argument = argv[index]
        if (argument === PublishDevBuild.UNGATED_FLAG) {
            options.ungated = true
            return index
        }
        if (!PublishDevBuild.OPTION_FLAGS.includes(argument)) {
            return PublishDevBuild.#readCommit(argument, index, options)
        }
        const value = argv[index + 1]
        PublishDevBuild.#require(value !== undefined, `missing value for ${argument}`)
        PublishDevBuild.#setOption(options, argument, value)
        return index + 1
    }

    /**
     * @param {string} argument
     * @param {number} index
     * @param {Options} options
     * @returns {number}
     */
    static #readCommit(argument, index, options) {
        PublishDevBuild.#require(
            !argument.startsWith('-'),
            `unknown option ${argument}\n${PublishDevBuild.USAGE}`
        )
        PublishDevBuild.#require(
            options.commit === '',
            `only one commit may be given\n${PublishDevBuild.USAGE}`
        )
        options.commit = argument
        return index
    }

    /**
     * @param {Options} options
     * @param {string} flag
     * @param {string} value
     * @returns {void}
     */
    static #setOption(options, flag, value) {
        switch (flag) {
            case '--lane':
                options.mode = 'lane'
                options.platform = value
                break
            case '--sha':
                options.commit = value
                break
            case '--out':
                options.out = path.resolve(value)
                break
            case '--gate-log':
                options.gateLog = path.resolve(value)
                break
            case '--contains':
                options.contains.push(value)
                break
            default:
                options.logs = path.resolve(value)
        }
    }

    /**
     * @param {Options} options
     * @returns {void}
     */
    static #requireOptions(options) {
        PublishDevBuild.#require(options.commit !== '', PublishDevBuild.USAGE)
        if (options.mode !== 'lane') return PublishDevBuild.#requireGateChoice(options)
        const known = PublishDevBuild.PLATFORMS.some(
            (platform) => platform.name === options.platform
        )
        PublishDevBuild.#require(known, `unknown platform ${options.platform}`)
        PublishDevBuild.#require(
            PublishDevBuild.COMMIT_PATTERN.test(options.commit),
            'a lane needs the full 40-character commit'
        )
        PublishDevBuild.#require(
            options.out !== '' && options.logs !== '',
            'a lane needs --out and --logs'
        )
    }

    /**
     * A publish names the log of the FYLO gates that passed for the commit, or says
     * `--ungated` on purpose.
     *
     * @param {Options} options
     * @returns {void}
     */
    static #requireGateChoice(options) {
        const one = (options.gateLog !== '') !== options.ungated
        PublishDevBuild.#require(
            one,
            `give exactly one of --gate-log or --ungated\n${PublishDevBuild.USAGE}`
        )
    }

    /**
     * @param {LaneReport} report
     * @param {string} digest
     * @param {Platform} platform
     * @param {string} commit
     * @returns {void}
     */
    static #checkReport(report, digest, platform, commit) {
        const identity = report.identity
        const where = `${platform.name}:`
        PublishDevBuild.#require(
            report.sha256 === digest,
            `${where} the copied file is ${digest}, the lane built ${report.sha256}`
        )
        PublishDevBuild.#require(
            identity.commit === commit,
            `${where} fylo reports commit ${identity.commit}, not ${commit}`
        )
        PublishDevBuild.#require(
            identity.buildTarget === platform.name,
            `${where} fylo reports target ${identity.buildTarget}`
        )
        PublishDevBuild.#require(
            identity.buildKind === PublishDevBuild.BUILD_KIND,
            `${where} fylo reports build kind ${identity.buildKind}`
        )
        PublishDevBuild.#require(
            report.target === platform.triple,
            `${where} rustc built for ${report.target}, not ${platform.triple}`
        )
        PublishDevBuild.#checkGlibc(report, platform)
    }

    /**
     * @param {LaneReport} report
     * @param {Platform} platform
     * @returns {void}
     */
    static #checkGlibc(report, platform) {
        if (platform.name !== PublishDevBuild.LINUX) return
        const glibc = report.glibc ?? ''
        const fits =
            /^\d+(\.\d+)+$/.test(glibc) &&
            PublishDevBuild.compareVersions(glibc, PublishDevBuild.MAX_GLIBC) <= 0
        PublishDevBuild.#require(
            fits,
            `${platform.name}: needs glibc ${glibc || 'unknown'}, more than ${PublishDevBuild.MAX_GLIBC}`
        )
    }

    /**
     * @param {LaneReport} report
     * @param {string} digest
     * @param {Platform} platform
     * @param {number} bytes
     * @returns {PlatformEntry}
     */
    static #entry(report, digest, platform, bytes) {
        return {
            file: platform.file,
            sha256: digest,
            bytes,
            rustc: report.rustc,
            rustTarget: report.target,
            version: report.identity.runtimeVersion,
            buildTarget: report.identity.buildTarget,
            ...(report.glibc === undefined ? {} : { glibc: report.glibc })
        }
    }

    /**
     * @param {string[]} values
     * @param {string} what
     * @returns {void}
     */
    static #requireOne(values, what) {
        const distinct = [...new Set(values)]
        PublishDevBuild.#require(
            distinct.length === 1,
            `the platforms report different ${what}: ${distinct.join(', ')}`
        )
    }

    /**
     * @param {string} clone
     * @param {string} commit
     * @param {Options} options
     * @returns {Promise<Provenance>}
     */
    static async #provenance(clone, commit, options) {
        const contains = options.contains.map((input) =>
            PublishDevBuild.#resolveAncestor(clone, input, commit)
        )
        await PublishDevBuild.#requireGateLog(options.gateLog)
        return { gateLog: options.gateLog, contains }
    }

    /**
     * @param {string} gateLog
     * @returns {Promise<void>}
     */
    static async #requireGateLog(gateLog) {
        if (gateLog === '') return
        PublishDevBuild.#require(
            await PublishDevBuild.#exists(gateLog),
            `no gate log at ${gateLog}`
        )
    }

    /**
     * @param {string} clone
     * @param {string} input
     * @param {string} commit
     * @returns {string}
     */
    static #resolveAncestor(clone, input, commit) {
        const ancestor = PublishDevBuild.#git(clone, [
            'rev-parse',
            '--verify',
            `${input}^{commit}`
        ]).trim()
        const result = spawnSync('git', [
            '-C',
            clone,
            'merge-base',
            '--is-ancestor',
            ancestor,
            commit
        ])
        PublishDevBuild.#require(result.status === 0, `${ancestor} is not contained in ${commit}`)
        return ancestor
    }

    /**
     * @param {string} directory
     * @returns {string}
     */
    static #rustHost(directory) {
        const text = PublishDevBuild.#capture(['rustc', '-vV'], directory)
        return /^host: (.+)$/m.exec(text)?.[1]?.trim() ?? ''
    }

    /**
     * Removes the staging folder and the lock when the publisher is stopped, so a stopped
     * run leaves nothing a reader or the next run could mistake for a build.
     *
     * @param {string[]} paths
     * @returns {void}
     */
    static #cleanUpOnSignal(paths) {
        const cleanUp = () => {
            for (const target of paths) rmSync(target, { recursive: true, force: true })
            process.exit(PublishDevBuild.SIGNAL_EXIT)
        }
        process.once('SIGTERM', cleanUp)
        process.once('SIGINT', cleanUp)
    }

    /**
     * @param {string} binary
     * @param {string} digest
     * @param {string} file
     * @returns {Promise<void>}
     */
    static async #writeChecksum(binary, digest, file) {
        const checksum = path.join(path.dirname(binary), PublishDevBuild.CHECKSUM)
        await writeFile(checksum, `${digest}  ${file}\n`)
        await chmod(checksum, PublishDevBuild.READ_ONLY_FILE)
        await chmod(binary, PublishDevBuild.READ_ONLY_EXECUTABLE)
    }

    /**
     * @param {string} staging
     * @param {Record<string, unknown>} manifest
     * @returns {Promise<void>}
     */
    static async #writeManifest(staging, manifest) {
        const file = path.join(staging, PublishDevBuild.MANIFEST)
        await writeFile(file, `${JSON.stringify(manifest, null, 4)}\n`)
        await chmod(file, PublishDevBuild.READ_ONLY_FILE)
    }

    /**
     * @param {string} staging
     * @returns {Promise<void>}
     */
    static async #makePlatformFolders(staging) {
        await rm(staging, { recursive: true, force: true })
        for (const platform of PublishDevBuild.PLATFORMS) {
            await mkdir(path.join(staging, platform.name), { recursive: true })
        }
    }

    /**
     * @param {string} root
     * @returns {Promise<void>}
     */
    static async #removeOldStaging(root) {
        const names = await readdir(root)
        for (const name of names.filter((entry) =>
            entry.startsWith(PublishDevBuild.STAGING_PREFIX)
        )) {
            await rm(path.join(root, name), { recursive: true, force: true })
        }
    }

    /**
     * @param {LaneResult[]} results
     * @param {string} logs
     * @returns {void}
     */
    static #requireLanesPassed(results, logs) {
        const failed = results.filter((result) => result.exitCode !== 0)
        const text = failed
            .map((result) => `${result.platform} (exit ${result.exitCode})`)
            .join(', ')
        PublishDevBuild.#require(
            failed.length === 0,
            `lanes failed: ${text}; nothing was published; logs in ${logs}`
        )
    }

    /**
     * @param {Platform} platform
     * @returns {string[]}
     */
    static #vmFlags(platform) {
        return platform.vm === '' ? [] : [`--vm=${platform.vm}`]
    }

    /**
     * @param {string} lock
     * @returns {Promise<void>}
     */
    static async #removeStaleLock(lock) {
        const owner = await PublishDevBuild.#readText(path.join(lock, PublishDevBuild.LOCK_OWNER))
        const pid = Number.parseInt(owner, 10)
        if (Number.isNaN(pid) || PublishDevBuild.#isAlive(pid)) return
        console.log(`publish-dev-build: removing the lock of stopped publisher ${pid}`)
        await rm(lock, { recursive: true, force: true })
    }

    /**
     * @param {string} lock
     * @returns {Promise<void>}
     */
    static async #createLock(lock) {
        try {
            await mkdir(lock)
        } catch (error) {
            const owner = await PublishDevBuild.#readText(
                path.join(lock, PublishDevBuild.LOCK_OWNER)
            )
            throw new Error(
                `another publisher holds ${lock} (${owner.trim() || 'no owner yet'}): ${PublishDevBuild.#message(error)}`
            )
        }
        await writeFile(path.join(lock, PublishDevBuild.LOCK_OWNER), `${process.pid}\n`)
    }

    /**
     * @param {number} pid
     * @returns {boolean}
     */
    static #isAlive(pid) {
        try {
            process.kill(pid, 0)
            return true
        } catch (error) {
            return /** @type {NodeJS.ErrnoException} */ (error).code === 'EPERM'
        }
    }

    /**
     * @param {string} file
     * @returns {Promise<string>}
     */
    static async #readText(file) {
        try {
            return await readFile(file, 'utf8')
        } catch {
            return ''
        }
    }

    /**
     * @param {string} final
     * @returns {Promise<void>}
     */
    static async #requireAbsent(final) {
        PublishDevBuild.#require(
            !(await PublishDevBuild.#exists(final)),
            `${final} already exists; a finished build is never replaced`
        )
    }

    /**
     * @param {string} final
     * @returns {number}
     */
    static #alreadyPublished(final) {
        console.log(`publish-dev-build: already published ${final}`)
        return 0
    }

    /**
     * @param {string} file
     * @returns {Promise<boolean>}
     */
    static async #exists(file) {
        try {
            await stat(file)
            return true
        } catch {
            return false
        }
    }

    /**
     * @param {string} directory
     * @returns {Promise<string>}
     */
    static async #existingRealPath(directory) {
        try {
            return await realpath(directory)
        } catch {
            return path.resolve(directory)
        }
    }

    /**
     * @param {string} child
     * @param {string} parent
     * @returns {boolean}
     */
    static #isInside(child, parent) {
        const relative = path.relative(parent, child)
        return relative === '' || !(relative.startsWith('..') || path.isAbsolute(relative))
    }

    /**
     * @param {string} logRoot
     * @param {string} commit
     * @returns {string}
     */
    static #logDirectory(logRoot, commit) {
        const now = new Date()
        const pad = (/** @type {number} */ value) => String(value).padStart(2, '0')
        const date = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}`
        const stamp = `${date}-${pad(now.getHours())}${pad(now.getMinutes())}`
        const short = commit.slice(0, PublishDevBuild.SHORT_COMMIT_LENGTH)
        return path.join(logRoot, 'FYLO', `publish-${stamp}-${short}`)
    }

    /** @returns {string} */
    static #logRoot() {
        const anvil = '/Volumes/ANVIL/Logs'
        const fallback = path.join(PublishDevBuild.HOME, 'Library', 'Logs', 'local')
        if (process.env.LOG_ROOT) return process.env.LOG_ROOT
        return spawnSync('test', ['-w', anvil]).status === 0 ? anvil : fallback
    }

    /**
     * @param {string} name
     * @param {string} fallback
     * @returns {string}
     */
    static #environment(name, fallback) {
        return process.env[name] || fallback
    }

    /**
     * @param {string[]} command
     * @param {string} cwd
     * @param {Record<string, string | undefined>} [env]
     * @returns {string}
     */
    static #capture(command, cwd, env = process.env) {
        const result = spawnSync(command[0], command.slice(1), { cwd, env, encoding: 'utf8' })
        const detail = result.error?.message ?? result.stderr
        PublishDevBuild.#require(
            result.status === 0,
            `${command.join(' ')} failed (exit ${result.status}): ${detail}`
        )
        return result.stdout
    }

    /**
     * @param {string[]} command
     * @param {StreamOptions} options
     * @returns {Promise<void>}
     */
    static async #checkedStream(command, options) {
        const exitCode = await PublishDevBuild.#stream(command, options)
        PublishDevBuild.#require(exitCode === 0, `${command.join(' ')} failed (exit ${exitCode})`)
    }

    /**
     * Runs a command, prints each output line (with an optional prefix) and appends it to an
     * optional log file.
     *
     * @param {string[]} command
     * @param {StreamOptions} options
     * @returns {Promise<number>}
     */
    static async #stream(command, options) {
        console.log(`${options.prefix ?? ''}$ ${command.join(' ')}`)
        const child = spawn(command[0], command.slice(1), {
            cwd: options.cwd,
            env: options.env ?? process.env,
            stdio: ['ignore', 'pipe', 'pipe']
        })
        const exited = new Promise((resolve) => {
            child.on('error', () => resolve(1))
            child.on('close', (code) => resolve(code ?? 1))
        })
        await Promise.all([
            PublishDevBuild.#pipe(child.stdout, options),
            PublishDevBuild.#pipe(child.stderr, options)
        ])
        return /** @type {number} */ (await exited)
    }

    /**
     * @param {import('node:stream').Readable} stream
     * @param {StreamOptions} options
     * @returns {Promise<void>}
     */
    static async #pipe(stream, options) {
        for await (const line of createInterface({ input: stream, crlfDelay: Infinity })) {
            console.log(`${options.prefix ?? ''}${line}`)
            if (options.log) await appendFile(options.log, `${line}\n`)
        }
    }

    /**
     * @param {boolean} condition
     * @param {string} message
     * @returns {void}
     */
    static #require(condition, message) {
        if (!condition) throw new Error(message)
    }

    /**
     * @param {unknown} error
     * @returns {string}
     */
    static #message(error) {
        return error instanceof Error ? error.message : String(error)
    }
}

if (import.meta.main) {
    await PublishDevBuild.main(process.argv.slice(2))
}
