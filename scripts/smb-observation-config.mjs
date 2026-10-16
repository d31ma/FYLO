import { randomUUID } from 'node:crypto'
import { isAbsolute, relative, resolve } from 'node:path'

export class SmbObservationConfig {
    static sourceUrl = import.meta.url

    static create(plan, options = {}) {
        return SmbObservationConfig.#create(plan, options)
    }

    static #create(
        plan,
        { backing = 'fat', retainFixture = false, runId = randomUUID(), now = Date.now() } = {}
    ) {
        SmbObservationConfig.#identity(plan, backing, runId)
        SmbObservationConfig.#paths(plan)
        SmbObservationConfig.#artifacts(plan.artifacts)
        SmbObservationConfig.#tools(plan.tools)
        const limits = SmbObservationConfig.#limits(plan.limits)
        const memoryMiB =
            2 * limits.clientMiB + limits.serverMiB + limits.azuriteMiB + limits.hostMiB
        const persistentMiB = 4 * limits.rootfsMiB + limits.evidenceMiB
        SmbObservationConfig.#admission(plan, limits, memoryMiB, persistentMiB, now)
        SmbObservationConfig.#disks(plan.diskFloors)
        return {
            ...plan,
            backing,
            retainFixture,
            runId,
            namespace: `fylo-smb-${backing}-${runId}`,
            limits,
            memoryMiB,
            persistentMiB,
            profile: 'bounded-synthetic-fat-or-tmpfs-v1'
        }
    }

    static #identity(plan, backing, runId) {
        SmbObservationConfig.#require(plan && typeof plan === 'object', 'A plan is required')
        SmbObservationConfig.#require(
            ['fat', 'tmpfs'].includes(backing),
            'backing must be fat or tmpfs'
        )
        SmbObservationConfig.#require(
            /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/.test(runId),
            'runId must be a generated UUID'
        )
        SmbObservationConfig.#require(
            /^[A-Za-z0-9_-]{1,64}$/.test(plan.owner ?? ''),
            'Invalid owner'
        )
        SmbObservationConfig.#require(
            /^[a-f0-9]{32}$/.test(plan.hostMachineId ?? ''),
            'Pin hostMachineId'
        )
        SmbObservationConfig.#require(
            /^[A-Za-z0-9:-]{12,100}$/.test(plan.dockerDaemonId ?? ''),
            'Pin dockerDaemonId'
        )
    }

    static #paths(plan) {
        for (const name of [
            'sourceRoot',
            'outputParent',
            'dockerSocket',
            'dockerRootPath',
            'hostCgroup'
        ]) {
            SmbObservationConfig.#path(plan[name], name)
        }
        const source = resolve(plan.sourceRoot)
        const output = resolve(plan.outputParent)
        const position = relative(source, output)
        SmbObservationConfig.#require(
            source !== '/' &&
                output !== '/' &&
                position !== '' &&
                (position === '..' || position.startsWith('../')),
            'Evidence must be outside the source tree'
        )
        SmbObservationConfig.#require(
            resolve(plan.hostCgroup).startsWith('/sys/fs/cgroup/'),
            'Use an existing bounded cgroup v2 scope'
        )
    }

    static #artifacts(artifacts = {}) {
        SmbObservationConfig.#require(
            /^v\d+\.\d+\.\d+$/.test(artifacts.nodeVersion ?? ''),
            'Pin nodeVersion'
        )
        SmbObservationConfig.#require(
            /^[a-f0-9]{40}$/.test(artifacts.sourceHead ?? ''),
            'Pin sourceHead'
        )
        for (const key of ['clientImage', 'azuriteImage']) {
            SmbObservationConfig.#require(
                /^sha256:[a-f0-9]{64}$/.test(artifacts[key] ?? ''),
                'Pin local image ID: ' + key
            )
        }
        for (const key of ['fylo', 'preview', 'http']) {
            SmbObservationConfig.#path(artifacts.binaries?.[key]?.path, 'binaries.' + key + '.path')
            SmbObservationConfig.#digest(
                artifacts.binaries?.[key]?.sha256,
                'binaries.' + key + '.sha256'
            )
        }
        SmbObservationConfig.#path(artifacts.provenance?.path, 'provenance.path')
        SmbObservationConfig.#digest(artifacts.provenance?.sha256, 'provenance.sha256')
        SmbObservationConfig.#sources(artifacts.sources)
    }

    static #sources(sources = {}) {
        const required = [
            'crates/fylo-storage-native/src/write.rs',
            'crates/fylo-storage-native/src/write/version.rs',
            'scripts/verify-serverless-local.mjs',
            'scripts/serverless-outcome-assessment.mjs',
            'scripts/verify-browser-wasm-host.mjs',
            'tests/scripts/serverless-outcome-assessment.test.js',
            'scripts/smb-observation-config.mjs',
            'scripts/smb-observation-evidence.mjs',
            'scripts/smb-observation-process.mjs',
            'scripts/smb-observation-journal.mjs',
            'scripts/smb-observation.mjs',
            'scripts/verify-smb-observation.mjs'
        ]
        for (const name of required) SmbObservationConfig.#digest(sources[name], name)
        for (const [name, digest] of Object.entries(sources)) {
            SmbObservationConfig.#require(
                /^[A-Za-z0-9_./-]+$/.test(name) &&
                    !isAbsolute(name) &&
                    !name.split('/').some((part) => ['.', '..', ''].includes(part)),
                'Unsafe source pin path'
            )
            SmbObservationConfig.#digest(digest, name)
        }
    }

    static #tools(tools = {}) {
        for (const key of ['bun', 'functions', 'dockerServer', 'python', 'containerd']) {
            SmbObservationConfig.#require(
                typeof tools[key] === 'string' && /^[\w.+-]{1,80}$/.test(tools[key]),
                'Pin tools.' + key
            )
        }
    }

    static #limits(overrides = {}) {
        const limits = {
            laneMs: 300000,
            totalMs: 420000,
            subprocessMs: 30000,
            requestMs: 25000,
            stopSeconds: 10,
            evidenceMiB: 100,
            rootfsMiB: 128,
            fixtureMiB: 128,
            fatImageMiB: 96,
            clientMiB: 1024,
            serverMiB: 256,
            azuriteMiB: 256,
            hostMiB: 1024,
            pids: 128,
            cpus: 1,
            ...overrides
        }
        for (const [name, value] of Object.entries(limits)) {
            SmbObservationConfig.#require(
                Number.isInteger(value) && value > 0,
                'Invalid limits.' + name
            )
        }
        SmbObservationConfig.#require(
            limits.laneMs <= 300000 &&
                limits.totalMs <= 900000 &&
                limits.totalMs >= limits.laneMs + 60000 &&
                limits.subprocessMs <= 30000 &&
                limits.requestMs > 4000 &&
                limits.requestMs <= limits.subprocessMs - 2000 &&
                limits.stopSeconds === 10 &&
                limits.evidenceMiB <= 100 &&
                limits.fixtureMiB <= 128 &&
                limits.fatImageMiB >= 64 &&
                limits.fatImageMiB < limits.fixtureMiB &&
                limits.rootfsMiB <= 128 &&
                limits.pids <= 128 &&
                limits.cpus === 1 &&
                limits.serverMiB >= limits.fixtureMiB + 64 &&
                limits.hostMiB >= 512,
            'Limits exceed the bounded synthetic observation profile'
        )
        return limits
    }

    static #admission(plan, limits, memoryMiB, persistentMiB, now) {
        const admission = plan.admission ?? {}
        const approved = Date.parse(admission.approvedAt)
        const expires = Date.parse(admission.expiresAt)
        SmbObservationConfig.#require(
            admission.projectIo === true &&
                admission.exclusiveSlot === true &&
                admission.healthReady === true &&
                admission.owner === plan.owner &&
                admission.healthOwner === '01a12c31-5976-747d-845c-00610048b0cc' &&
                admission.hostMachineId === plan.hostMachineId &&
                admission.dockerDaemonId === plan.dockerDaemonId &&
                approved <= now &&
                now - approved <= 300000 &&
                expires >= now + limits.totalMs &&
                expires - approved <= 900000,
            'Fresh host-bound health/I-O/slot admission valid through export is required'
        )
        SmbObservationConfig.#require(
            memoryMiB <= 6144 && persistentMiB <= 1024,
            'Reservation exceeds admission ceilings'
        )
        SmbObservationConfig.#require(
            Number.isInteger(admission.availableMiB) && admission.availableMiB >= memoryMiB + 2048,
            'Admission lacks one overlapping memory budget plus 2 GiB headroom'
        )
        SmbObservationConfig.#require(
            Array.isArray(admission.backingDisks) && admission.backingDisks.length > 0,
            'Backing disk receipts are required'
        )
        for (const disk of admission.backingDisks) {
            SmbObservationConfig.#require(
                typeof disk.volume === 'string' &&
                    Number.isInteger(disk.floorMiB) &&
                    disk.floorMiB >= 0 &&
                    Number.isInteger(disk.freeMiB) &&
                    disk.freeMiB >= disk.floorMiB + persistentMiB,
                'Backing disk floor/growth admission failed'
            )
        }
    }

    static #disks(disks) {
        SmbObservationConfig.#require(
            Array.isArray(disks) && disks.length > 0,
            'Actual Linux backing-path disk floors are required'
        )
        for (const disk of disks) {
            SmbObservationConfig.#path(disk.path, 'diskFloors.path')
            SmbObservationConfig.#require(
                Number.isInteger(disk.floorMiB) && disk.floorMiB >= 0,
                'Invalid disk floor'
            )
        }
    }

    static #path(value, name) {
        SmbObservationConfig.#require(
            typeof value === 'string' && isAbsolute(value) && !/[\x00-\x1f,:]/.test(value),
            'Invalid absolute ' + name
        )
    }

    static #digest(value, name) {
        SmbObservationConfig.#require(
            /^[a-f0-9]{64}$/.test(value ?? ''),
            'SHA256 pin required: ' + name
        )
    }

    static #require(condition, message) {
        if (!condition) throw new Error(message)
    }
}
