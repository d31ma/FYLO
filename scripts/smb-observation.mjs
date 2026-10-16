import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { access, mkdir, readFile, realpath, statfs, writeFile } from 'node:fs/promises'
import { isDeepStrictEqual, parseArgs } from 'node:util'
import { join, relative, resolve } from 'node:path'
import { performance } from 'node:perf_hooks'
import { fileURLToPath } from 'node:url'
import { SmbObservationConfig } from './smb-observation-config.mjs'
import { SmbObservationEvidence } from './smb-observation-evidence.mjs'
import { SmbObservationJournal } from './smb-observation-journal.mjs'
import { SmbObservationProcess } from './smb-observation-process.mjs'

export class SmbObservation {
    static async cli(argv) {
        return SmbObservation.#cli(argv)
    }

    static async finish(options) {
        return SmbObservation.#finish(options)
    }

    constructor(config) {
        this.config = config
        this.startedAt = Date.now()
        this.startedMonotonicMs = performance.now()
        this.directory = join(config.outputParent, config.namespace)
        this.root = `/mnt/state/observation/${config.runId}/root`
        this.shareRoot = `/share/observation/${config.runId}/root`
        this.resources = []
        this.labels = {
            'fylo.observation.run': config.runId,
            'fylo.observation.owner': config.owner
        }
        this.journal = new SmbObservationJournal(
            this.directory,
            config.limits.evidenceMiB * 1024 * 1024,
            this.startedAt + config.limits.totalMs
        )
        this.commands = new SmbObservationProcess(this.journal, config, this.startedAt)
    }

    async run() {
        return this.#run()
    }

    static async #cli(argv) {
        const { values } = parseArgs({
            args: argv,
            options: {
                plan: { type: 'string' },
                backing: { type: 'string', default: 'fat' },
                'retain-fixture': { type: 'boolean', default: false },
                help: { type: 'boolean' }
            }
        })
        if (values.help) {
            console.log(
                'node scripts/verify-smb-observation.mjs --plan /absolute/plan.json [--backing fat|tmpfs] [--retain-fixture]\nRequires fresh admission, local pinned images/binaries/tool versions, Docker quotas and a bounded host cgroup. No builds, pulls, retries or recovery. One synthetic lane only.'
            )
            return
        }
        if (!values.plan || !values.plan.startsWith('/'))
            throw new Error('An absolute --plan is required')
        const plan = JSON.parse(await readFile(values.plan, 'utf8'))
        const config = SmbObservationConfig.create(plan, {
            backing: values.backing,
            retainFixture: values['retain-fixture']
        })
        return new SmbObservation(config).run()
    }

    static async #finish({ exportEvidence, cleanup, retainFixture = false, uncertain = false }) {
        await exportEvidence()
        const retained = retainFixture || uncertain
        if (!retained) await cleanup()
        return { exported: true, retained }
    }

    async #run() {
        await this.#boundedLocal(() => this.#localPrerequisites())
        await this.journal.initialize({
            ...this.config,
            sourcePins: this.sourcePins,
            binaryPins: this.binaryPins,
            node: process.version,
            fixtureRoot: this.root,
            namespace: this.config.namespace,
            mountOptions: this.#mountOptions(),
            profileMeaning:
                'Reduced synthetic FAT/tmpfs comparison; not reproduction of the original 3 GiB fixture or general SMB qualification'
        })
        let failure
        try {
            await this.#dockerPrerequisites()
            await this.#startResources()
            await this.#fencing()
            await this.#startFunctions()
            await this.#warm()
            await this.#observe()
            await this.#diagnostics('final')
        } catch (error) {
            failure = error
            await this.journal.record({
                type: 'failure',
                observedAt: new Date().toISOString(),
                error: this.#error(error),
                uncertain: this.commands.uncertain,
                resources: this.resources
            })
            if (!this.commands.uncertain)
                await this.#diagnostics('failure').catch((diagnosticError) => {
                    failure = new AggregateError(
                        [error, diagnosticError],
                        'Observation and diagnostic export failed'
                    )
                })
        }
        const receipt = await SmbObservation.finish({
            exportEvidence: () => this.journal.export(),
            cleanup: () => this.#cleanup(),
            retainFixture: this.config.retainFixture || Boolean(failure) || !this.#successful(),
            uncertain: this.commands.uncertain
        })
        console.log(
            JSON.stringify({
                namespace: this.config.namespace,
                directory: this.directory,
                ...receipt
            })
        )
        if (failure) throw failure
        if (!this.#successful()) {
            process.exitCode = 1
        }
        return { ...receipt, assessment: this.assessment }
    }

    async #localPrerequisites() {
        if (process.platform !== 'linux')
            throw new Error('Execution requires the explicitly admitted Linux host')
        if (process.version !== this.config.artifacts.nodeVersion)
            throw new Error('Executing Node version does not match its pin')
        const source = await realpath(this.config.sourceRoot)
        const output = await realpath(this.config.outputParent)
        const position = relative(source, output)
        if (
            source === '/' ||
            output === '/' ||
            position === '' ||
            !(position === '..' || position.startsWith('../'))
        ) {
            throw new Error(
                'Evidence parent must be outside the source tree and cannot be a filesystem root'
            )
        }
        this.config.sourceRoot = source
        this.config.outputParent = output
        this.directory = join(output, this.config.namespace)
        this.journal.directory = this.directory
        this.sourcePins = await this.#pins(this.config.artifacts.sources, source)
        await this.#loadedSources(source)
        this.binaryPins = await this.#binaryPins()
        await this.#provenance()
        if (!this.config.diskFloors.some((disk) => resolve(disk.path) === output))
            throw new Error('Evidence backing path requires its own actual disk floor')
        await this.#diskFloors(true)
        const machine = (await readFile('/etc/machine-id', 'utf8')).trim()
        if (machine !== this.config.hostMachineId)
            throw new Error('Host identity does not match admission')
        await this.#hostCgroup()
        try {
            await access(this.directory)
        } catch (error) {
            if (error.code === 'ENOENT') return
            throw error
        }
        throw new Error('Evidence namespace collision')
    }

    async #loadedSources(source) {
        const modules = [
            [fileURLToPath(import.meta.url), 'scripts/smb-observation.mjs'],
            [process.argv[1], 'scripts/verify-smb-observation.mjs'],
            [fileURLToPath(SmbObservationConfig.sourceUrl), 'scripts/smb-observation-config.mjs'],
            [
                fileURLToPath(SmbObservationEvidence.sourceUrl),
                'scripts/smb-observation-evidence.mjs'
            ],
            [fileURLToPath(SmbObservationProcess.sourceUrl), 'scripts/smb-observation-process.mjs'],
            [fileURLToPath(SmbObservationJournal.sourceUrl), 'scripts/smb-observation-journal.mjs']
        ]
        for (const [actual, name] of modules) await this.#sourceBinding(actual, join(source, name))
    }

    async #sourceBinding(actual, expected) {
        if ((await realpath(actual)) !== (await realpath(expected)))
            throw new Error('Executing modules must belong to the pinned source tree')
    }

    async #provenance() {
        const pin = this.config.artifacts.provenance
        if ((await this.#hash(pin.path)) !== pin.sha256)
            throw new Error('Binary provenance receipt pin mismatch')
        const provenance = JSON.parse(await readFile(pin.path, 'utf8'))
        if (
            provenance.sourceHead !== this.config.artifacts.sourceHead ||
            !isDeepStrictEqual(provenance.binaries, this.binaryPins)
        )
            throw new Error('Provenance receipt does not bind these binaries to the source HEAD')
        for (const name of [
            'crates/fylo-storage-native/src/write.rs',
            'crates/fylo-storage-native/src/write/version.rs'
        ]) {
            if (provenance.sources?.[name] !== this.sourcePins[name])
                throw new Error('Native source provenance mismatch')
        }
    }

    async #diskFloors(reserve = false) {
        for (const disk of this.config.diskFloors) {
            const filesystem = await statfs(disk.path)
            const freeMiB = (filesystem.bavail * filesystem.bsize) / (1024 * 1024)
            if (freeMiB < disk.floorMiB + (reserve ? this.config.persistentMiB : 0))
                throw new Error(`Insufficient disk floor at ${disk.path}`)
        }
    }

    async #boundedLocal(operation) {
        const remaining = Math.min(
            30000,
            this.config.limits.totalMs - (performance.now() - this.startedMonotonicMs)
        )
        if (remaining <= 0) throw new Error('Local prerequisite/export deadline exhausted')
        let timer
        try {
            return await Promise.race([
                operation(),
                new Promise((_, reject) => {
                    timer = setTimeout(
                        () =>
                            reject(new Error('Local I/O timeout; stop and defer health recovery')),
                        remaining
                    )
                })
            ])
        } catch (error) {
            this.commands.uncertain = true
            throw error
        } finally {
            clearTimeout(timer)
        }
    }

    async #guard(includeA = false) {
        await this.#boundedLocal(async () => {
            if (Date.parse(this.config.admission.expiresAt) <= Date.now())
                throw new Error('Admission expired')
            await this.#diskFloors()
            await this.#hostCgroup()
            const meminfo = await readFile('/proc/meminfo', 'utf8')
            const availableKiB = Number(/^MemAvailable:\s+(\d+)\s+kB$/m.exec(meminfo)?.[1])
            if (!Number.isFinite(availableKiB) || availableKiB < 2048 * 1024)
                throw new Error('Shared memory headroom fell below 2 GiB')
        })
        for (const role of includeA ? ['smb', 'azurite', 'a', 'b'] : ['smb', 'azurite', 'b']) {
            const state = (await this.#inspect(role)).State
            if (!state.Running || state.OOMKilled)
                throw new Error('Required fixture service is unhealthy; no further mutations')
        }
        await this.journal.record({
            type: 'capacity-health-gate',
            observedAt: new Date().toISOString()
        })
    }

    async #hostCgroup() {
        const membership = (await readFile('/proc/self/cgroup', 'utf8')).trim()
        const path = membership
            .split('\n')
            .find((line) => line.startsWith('0::'))
            ?.slice(3)
        if (!path || resolve('/sys/fs/cgroup' + path) !== this.config.hostCgroup)
            throw new Error('Runner must already be inside its admitted cgroup v2 scope')
        const memory = Number(
            (await readFile(join(this.config.hostCgroup, 'memory.max'), 'utf8')).trim()
        )
        const swap = Number(
            (await readFile(join(this.config.hostCgroup, 'memory.swap.max'), 'utf8')).trim()
        )
        const pids = Number(
            (await readFile(join(this.config.hostCgroup, 'pids.max'), 'utf8')).trim()
        )
        const [quota, period] = (await readFile(join(this.config.hostCgroup, 'cpu.max'), 'utf8'))
            .trim()
            .split(/\s+/)
            .map(Number)
        if (
            !Number.isFinite(memory) ||
            memory > this.config.limits.hostMiB * 1024 * 1024 ||
            swap !== 0 ||
            !Number.isFinite(pids) ||
            pids > 256 ||
            !Number.isFinite(quota) ||
            quota > period
        ) {
            throw new Error(
                'Host cgroup must enforce admitted memory, no swap, at most256 pids and1 CPU'
            )
        }
    }

    async #pins(pins, parent) {
        const actual = {}
        for (const [name, expected] of Object.entries(pins)) {
            const filename = await realpath(join(parent, name))
            if (relative(parent, filename).startsWith('..'))
                throw new Error('Source pin escapes source root')
            actual[name] = await this.#hash(filename)
            if (actual[name] !== expected) throw new Error(`Source pin mismatch: ${name}`)
        }
        return actual
    }

    async #binaryPins() {
        const pins = {}
        for (const [name, binary] of Object.entries(this.config.artifacts.binaries)) {
            pins[name] = await this.#hash(await realpath(binary.path))
            if (pins[name] !== binary.sha256) throw new Error(`Binary pin mismatch: ${name}`)
        }
        return pins
    }

    async #hash(filename) {
        const hash = createHash('sha256')
        for await (const chunk of createReadStream(filename, {
            signal: AbortSignal.timeout(30000)
        }))
            hash.update(chunk)
        return hash.digest('hex')
    }

    async #dockerPrerequisites() {
        if (process.env.DOCKER_CONTEXT || process.env.DOCKER_HOST || process.env.DOCKER_TLS_VERIFY)
            throw new Error(
                'Environment Docker route overrides require separate admission; no route substitution'
            )
        const context = await this.commands.execute(['docker', 'context', 'inspect'])
        this.#requireExit(context)
        const contexts = JSON.parse(context.stdout)
        if (
            contexts.length !== 1 ||
            contexts[0].Endpoints?.docker?.Host !== `unix://${this.config.dockerSocket}`
        )
            throw new Error(
                'Current Docker endpoint differs from the admitted local socket; no route substitution'
            )
        const info = JSON.parse((await this.#docker(['info', '--format', '{{json .}}'])).stdout)
        if (
            info.ID !== this.config.dockerDaemonId ||
            info.DockerRootDir !== this.config.dockerRootPath
        )
            throw new Error('Docker daemon/backing identity mismatch')
        if (
            !this.config.diskFloors.some(
                (disk) => resolve(disk.path) === resolve(info.DockerRootDir)
            )
        )
            throw new Error('Docker backing path must have an actual disk floor')
        const head = await this.commands.execute([
            'git',
            '-C',
            this.config.sourceRoot,
            'rev-parse',
            'HEAD'
        ])
        this.#requireExit(head)
        if (head.stdout.trim() !== this.config.artifacts.sourceHead)
            throw new Error('Source HEAD drift')
        this.#requireExit(
            await this.commands.execute([
                'git',
                '-C',
                this.config.sourceRoot,
                'status',
                '--porcelain=v1'
            ])
        )
        const version = await this.#docker(['version', '--format', '{{.Server.Version}}'])
        if (version.stdout.trim() !== this.config.tools.dockerServer)
            throw new Error('Docker server version mismatch')
        const components = JSON.parse(
            (await this.#docker(['version', '--format', '{{json .Server.Components}}'])).stdout
        )
        if (
            !components.some(
                (component) =>
                    component.Name === 'containerd' &&
                    component.Version === this.config.tools.containerd
            )
        )
            throw new Error('Required containerd version/readiness is unverified')
        for (const image of [
            this.config.artifacts.clientImage,
            this.config.artifacts.azuriteImage
        ]) {
            const inspect = JSON.parse((await this.#docker(['image', 'inspect', image])).stdout)
            if (inspect.length !== 1 || inspect[0].Id !== image)
                throw new Error('Pinned local image missing')
        }
        for (const name of ['network', 'smb', 'azurite', 'a', 'b']) {
            const kind = name === 'network' ? 'network' : 'container'
            const inspect = await this.#docker([kind, 'inspect', this.#name(name)], {
                check: false
            })
            if (inspect.exitCode === 0) throw new Error('Docker namespace collision')
            if (!/No such (object|container|network)/i.test(inspect.stderr))
                throw new Error('Cannot prove namespace absence')
        }
    }

    async #docker(args, { check = true, ...options } = {}) {
        const transport = await this.commands.execute(
            ['docker', '--host', `unix://${this.config.dockerSocket}`, ...args],
            options
        )
        if (check) this.#requireExit(transport)
        return transport
    }

    #requireExit(transport) {
        if (
            transport.exitCode !== 0 ||
            transport.timedOut ||
            transport.error ||
            transport.evidenceIncomplete
        )
            throw new Error('Operation did not complete successfully; see full retained transport')
    }

    #name(role) {
        return `${this.config.namespace}-${role}`
    }

    #labelArgs() {
        return Object.entries(this.labels).flatMap(([name, value]) => [
            '--label',
            `${name}=${value}`
        ])
    }

    #mountOptions() {
        return 'vers=3.1.1,username=fylo,password=fylo,nounix,mfsymlinks,cache=strict,actimeo=30,nosharesock,uid=0,gid=0,file_mode=0600,dir_mode=0755'
    }

    #caps(memoryMiB) {
        const limits = this.config.limits
        return [
            '--memory',
            `${memoryMiB}m`,
            '--memory-swap',
            `${memoryMiB}m`,
            '--cpus',
            String(limits.cpus),
            '--pids-limit',
            String(limits.pids),
            '--storage-opt',
            `size=${limits.rootfsMiB}m`,
            '--tmpfs',
            '/tmp:size=32m,mode=1777',
            '--log-driver',
            'none'
        ]
    }

    async #startResources() {
        const network = await this.#docker([
            'network',
            'create',
            '--internal',
            ...this.#labelArgs(),
            this.#name('network')
        ])
        this.resources.push({ kind: 'network', role: 'network', id: network.stdout.trim() })
        await this.#create('smb', [
            '--privileged',
            ...this.#caps(this.config.limits.serverMiB),
            '--tmpfs',
            `/disk:size=${this.config.limits.fixtureMiB}m`,
            this.config.artifacts.clientImage,
            'sh',
            '-c',
            this.#sambaScript()
        ])
        await this.#ready('smb', 'READY')
        await this.#create('azurite', [
            '--user',
            '0:0',
            ...this.#caps(this.config.limits.azuriteMiB),
            this.config.artifacts.azuriteImage,
            'sh',
            '-c',
            'exec azurite --blobHost 0.0.0.0 --queueHost 0.0.0.0 --tableHost 0.0.0.0 --location /tmp/azurite > /var/log/observation.log 2>&1'
        ])
        await this.#boundedLocal(() => this.#application())
        for (const role of ['a', 'b']) await this.#create(role, this.#clientArgs(role))
        for (const role of ['a', 'b']) await this.#ready(role, 'MOUNT_READY')
        await this.#toolPins()
        await this.#linkProbe()
    }

    async #create(role, args) {
        const created = await this.#docker([
            'create',
            '--pull=never',
            '--name',
            this.#name(role),
            '--hostname',
            role,
            '--network',
            this.#name('network'),
            ...this.#labelArgs(),
            ...args
        ])
        const id = created.stdout.trim()
        if (!/^[a-f0-9]{64}$/.test(id))
            throw new Error('Unexpected resource ID; stop without broad cleanup')
        this.resources.push({ kind: 'container', role, id })
        await this.#docker(['start', id])
    }

    #sambaScript() {
        const config =
            '[global]\nserver min protocol = SMB3\nea support = no\nunix extensions = no\nstore dos attributes = no\nmap archive = no\n[state]\npath = /share\nread only = no\nforce user = root\n'
        const backing =
            this.config.backing === 'fat'
                ? `truncate -s ${this.config.limits.fatImageMiB}M /disk/share.img && mkfs.vfat -F 32 /disk/share.img && mkdir -p /share && mount -o loop,uid=0,gid=0,fmask=0077,dmask=0022 /disk/share.img /share`
                : 'mkdir -p /share && mount --bind /disk /share && chmod 700 /share'
        return `exec > /var/log/observation.log 2>&1; set -eu; printf '%s' '${config}' > /etc/samba/smb.conf; ${backing}; useradd -M fylo; printf 'fylo\\nfylo\\n' | smbpasswd -a -s fylo; echo READY; exec smbd --foreground --no-process-group --debug-stdout`
    }

    #clientArgs(role) {
        const binaries = this.config.artifacts.binaries
        const bindings = Object.entries({
            'fylo-rust': binaries.fylo.path,
            'fylo-write-preview': binaries.preview.path,
            http_invocation: binaries.http.path
        }).flatMap(([name, source]) => [
            '--mount',
            `type=bind,source=${source},target=/opt/fylo/${name},readonly`
        ])
        const script = `exec > /var/log/observation.log 2>&1; set -eu; mkdir -p /mnt/state; mount -t cifs //${this.#name('smb')}/state /mnt/state -o ${this.#mountOptions()}; mkdir -p ${this.root}; echo MOUNT_READY; while [ ! -f /tmp/start-functions ]; do sleep 0.1; done; exec func start --port 7071`
        return [
            '--privileged',
            '--stop-signal',
            'SIGTERM',
            ...this.#caps(this.config.limits.clientMiB),
            ...bindings,
            '--mount',
            `type=bind,source=${join(this.directory, 'app')},target=/app,readonly`,
            '--workdir',
            '/app',
            '--env',
            'FUNCTIONS_WORKER_RUNTIME=custom',
            '--env',
            'FUNCTIONS_CORE_TOOLS_TELEMETRY_OPTOUT=1',
            '--env',
            `AzureWebJobsStorage=${this.#azurite()}`,
            '--env',
            `FYLO_ROOT=${this.root}`,
            '--env',
            `FYLO_REQUEST_TIMEOUT_MS=${this.config.limits.requestMs - 2000}`,
            this.config.artifacts.clientImage,
            'sh',
            '-c',
            script
        ]
    }

    #azurite() {
        const key =
            'Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=='
        return `DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey=${key};BlobEndpoint=http://${this.#name('azurite')}:10000/devstoreaccount1;QueueEndpoint=http://${this.#name('azurite')}:10001/devstoreaccount1;TableEndpoint=http://${this.#name('azurite')}:10002/devstoreaccount1`
    }

    async #application() {
        await mkdir(join(this.directory, 'app/fylo'), { recursive: true, mode: 0o700 })
        await writeFile(
            join(this.directory, 'app/host.json'),
            JSON.stringify({
                version: '2.0',
                customHandler: {
                    description: { defaultExecutablePath: '/opt/fylo/http_invocation' },
                    enableForwardingHttpRequest: true
                }
            })
        )
        await writeFile(
            join(this.directory, 'app/fylo/function.json'),
            JSON.stringify({
                bindings: [
                    {
                        type: 'httpTrigger',
                        authLevel: 'anonymous',
                        direction: 'in',
                        name: 'req',
                        methods: ['post']
                    },
                    { type: 'http', direction: 'out', name: 'res' }
                ]
            })
        )
    }

    async #ready(role, marker) {
        const started = performance.now()
        while (performance.now() - started < 60000) {
            const info = await this.#inspect(role)
            if (!info.State.Running || info.State.OOMKilled)
                throw new Error(`Unhealthy ${role}; no readiness admission`)
            const logs = await this.#docker(
                ['exec', this.#resource(role).id, 'cat', '/var/log/observation.log'],
                { check: false }
            )
            if (logs.stdout.includes(marker) || logs.stderr.includes(marker)) return
            await new Promise((resolve) => setTimeout(resolve, 250))
        }
        throw new Error(`Readiness deadline expired: ${role}`)
    }

    async #toolPins() {
        const tools = await this.#docker([
            'exec',
            this.#resource('a').id,
            'sh',
            '-c',
            'set -eu; command -v curl mount.cifs timeout tar python3 func bun mkfs.vfat; printf "BUN="; bun --version; printf "FUNC="; func --version; printf "PYTHON="; python3 -c "import platform; print(platform.python_version())"'
        ])
        for (const [prefix, expected] of [
            ['BUN', this.config.tools.bun],
            ['FUNC', this.config.tools.functions],
            ['PYTHON', this.config.tools.python]
        ]) {
            if (!tools.stdout.split('\n').includes(`${prefix}=${expected}`))
                throw new Error(`Tool pin mismatch: ${prefix}`)
        }
        for (const role of ['a', 'b'])
            await this.#docker(['exec', this.#resource(role).id, 'cat', '/proc/mounts'])
    }

    async #linkProbe() {
        const script =
            'import json,os\np="/mnt/state/observation/' +
            this.config.runId +
            '/link-probe"\nopen(p,"x").close()\ntry:\n os.link(p,p+".link"); print(json.dumps({"linked":True,"errno":None}))\nexcept OSError as e:\n print(json.dumps({"linked":False,"errno":e.errno,"message":str(e)}))\n'
        const transport = await this.#docker([
            'exec',
            this.#resource('a').id,
            'python3',
            '-c',
            script
        ])
        const probe = JSON.parse(transport.stdout)
        await this.journal.record({ type: 'link-probe', probe, transport })
        if (this.config.backing === 'tmpfs' && (probe.linked !== true || probe.errno !== null))
            throw new Error('Tmpfs hard-link capability failed')
        if (
            this.config.backing === 'fat' &&
            (probe.linked !== false || ![1, 13, 95].includes(probe.errno))
        )
            throw new Error('FAT refusal is not a recognized link-unsupported errno')
    }

    async #fencing() {
        const args = [
            'docker',
            '--host',
            `unix://${this.config.dockerSocket}`,
            'exec',
            '-i',
            this.#resource('a').id,
            'timeout',
            '25s',
            '/opt/fylo/fylo-rust',
            'exec',
            '--loop',
            '--exclusive-root',
            '--filesystem-profile',
            'efs-sidecar-v1',
            '--root',
            this.root
        ]
        const holder = this.commands.start(args, { input: '{"op":"handshake"}\n', holdInput: true })
        const refusal = this.#fencingRefusal(holder).finally(() => holder.closeInput())
        const [refusalResult, holderResult] = await Promise.allSettled([refusal, holder.completion])
        const refused = this.#settledValue(refusalResult)
        const held = this.#settledValue(holderResult)
        this.#requireExit(held)
        const frame = JSON.parse(refused.stdout.trim())
        await this.journal.record({ type: 'fencing', holder: held, refused, frame })
        if (refused.timedOut || frame.ok !== false || frame.error?.code !== 'EROOTLOCKED')
            throw new Error('Cross-client EROOTLOCKED fencing gate failed; workloads forbidden')
        const takeover = await this.#docker(
            [
                'exec',
                '-i',
                this.#resource('b').id,
                'timeout',
                '25s',
                '/opt/fylo/fylo-rust',
                'exec',
                '--loop',
                '--exclusive-root',
                '--filesystem-profile',
                'efs-sidecar-v1',
                '--root',
                this.root
            ],
            { input: '{"op":"handshake"}\n' }
        )
        if (JSON.parse(takeover.stdout.trim()).ok !== true)
            throw new Error('Fencing holder release was not verified')
    }

    async #fencingRefusal(holder) {
        const start = performance.now()
        while (!holder.state.stdout.includes('\n') && performance.now() - start < 10000)
            await new Promise((resolve) => setTimeout(resolve, 50))
        const handshake = JSON.parse(holder.state.stdout.split('\n')[0])
        if (handshake.ok !== true) throw new Error('Root holder did not acquire fencing')
        return this.#docker(
            [
                'exec',
                '-i',
                this.#resource('b').id,
                'timeout',
                '25s',
                '/opt/fylo/fylo-rust',
                'exec',
                '--loop',
                '--exclusive-root',
                '--filesystem-profile',
                'efs-sidecar-v1',
                '--root',
                this.root
            ],
            { check: false, input: '{"op":"handshake"}\n' }
        )
    }

    #settledValue(result) {
        if (result.status === 'rejected') throw result.reason
        return result.value
    }

    async #startFunctions() {
        for (const role of ['a', 'b'])
            await this.#docker(['exec', this.#resource(role).id, 'touch', '/tmp/start-functions'])
        for (const role of ['a', 'b']) {
            const start = performance.now()
            while (performance.now() - start < 60000) {
                const logs = await this.#docker(
                    ['exec', this.#resource(role).id, 'cat', '/var/log/observation.log'],
                    { check: false }
                )
                if (logs.stdout.includes('Host lock lease acquired')) break
                if (!(await this.#inspect(role)).State.Running)
                    throw new Error('Functions host exited')
                await new Promise((resolve) => setTimeout(resolve, 250))
            }
            const probe = await this.#invoke(role, { op: 'handshake', root: this.root })
            if (probe.decoded.response?.outcome?.ok !== true || !probe.decoded.complete)
                throw new Error('Functions readiness failed')
        }
    }

    async #invoke(role, operation) {
        const request = { ...operation, root: this.root }
        const correlationId = `${this.config.runId}-${role}-${(this.sequence = (this.sequence ?? 0) + 1)}`
        await this.journal.record({
            type: 'request-start',
            correlationId,
            target: role,
            request,
            observedAt: new Date().toISOString()
        })
        const transport = await this.#docker(
            [
                'exec',
                '-i',
                this.#resource(role).id,
                'curl',
                '--include',
                '--silent',
                '--show-error',
                '--max-time',
                String(this.config.limits.requestMs / 1000),
                '--max-redirs',
                '0',
                '--request',
                'POST',
                '--header',
                'Content-Type: application/json',
                '--header',
                'Expect:',
                '--header',
                `X-Fylo-Observation-Id: ${correlationId}`,
                '--data-binary',
                '@-',
                'http://localhost:7071/api/fylo'
            ],
            {
                check: false,
                input: JSON.stringify(request),
                timeoutMs: this.config.limits.subprocessMs
            }
        )
        const decoded = SmbObservationEvidence.decode(transport, request)
        await this.journal.record({
            type: 'request-settled',
            correlationId,
            target: role,
            request,
            transport,
            decoded,
            observedAt: new Date().toISOString()
        })
        return { request, transport, decoded, correlationId }
    }

    async #warm() {
        await this.#guard(true)
        const created = await this.#invoke('a', {
            op: 'createCollection',
            collection: 'records',
            kind: 'document'
        })
        if (!created.decoded.complete || created.decoded.response?.outcome?.ok !== true)
            throw new Error('Synthetic collection creation failed')
        const written = await this.#invoke('a', {
            op: 'putData',
            collection: 'records',
            id: '4VRNF52JPCO',
            data: { name: 'Ada' }
        })
        if (!SmbObservationEvidence.acknowledged(written.request, written.transport))
            throw new Error('Warm write was not acknowledged')
        const read = await this.#invoke('b', {
            op: 'getDoc',
            collection: 'records',
            id: '4VRNF52JPCO'
        })
        if (
            !read.decoded.complete ||
            read.decoded.response?.outcome?.ok !== true ||
            !isDeepStrictEqual(read.decoded.response.outcome.result?.['4VRNF52JPCO'], {
                name: 'Ada'
            })
        )
            throw new Error('Cross-client warm read failed')
    }

    async #observe() {
        await this.#guard(true)
        const requests = Array.from({ length: 12 }, (_, index) => {
            const id = `4VRNF52JR${index.toString(16).toUpperCase()}0`
            return { op: 'putData', root: this.root, collection: 'records', id, data: { name: id } }
        })
        await this.journal.record({ type: 'burst-plan', requests })
        const pending = requests.map((request) => this.#invoke('a', request))
        const settled = Promise.allSettled(pending)
        await new Promise((resolve) => setTimeout(resolve, 50))
        const stopRequestedAt = new Date().toISOString()
        await this.journal.record({
            type: 'stop-request',
            target: this.#resource('a'),
            stopRequestedAt,
            graceSeconds: 10
        })
        const stop = await this.#docker(['stop', '--time', '10', this.#resource('a').id], {
            check: false
        })
        const outcomes = await settled
        await this.journal.record({
            type: 'burst-settled',
            outcomes: outcomes.map((outcome, index) => ({
                request: requests[index],
                status: outcome.status,
                value: outcome.value,
                reason: outcome.status === 'rejected' ? this.#error(outcome.reason) : null
            }))
        })
        const writes = outcomes
            .filter((outcome) => outcome.status === 'fulfilled')
            .map((outcome) => outcome.value)
        const acknowledged = writes
            .filter((write) => SmbObservationEvidence.acknowledged(write.request, write.transport))
            .map((write) => ({
                id: write.request.id,
                observedAt: write.transport.completedAt,
                observedMonotonicNs: write.transport.settledMonotonicNs
            }))
        await this.journal.record({
            type: 'acknowledgments',
            acknowledged,
            stopRequestedAt,
            stopTransport: stop,
            meaning: 'Client-observed completions; no server commitment timing inferred'
        })
        this.#requireExit(stop)
        const state = await this.#inspect('a')
        await this.journal.record({
            type: 'stop-state',
            stopRequestedAt,
            stopCompletedAt: stop.completedAt,
            transport: stop,
            state: state.State,
            requestedSignal: state.Config.StopSignal,
            exitSignalConvention: state.State.ExitCode >= 128 ? state.State.ExitCode - 128 : null,
            cleanTerminationEstablished: false,
            meaning:
                'Exit status is recorded; it does not establish whether Docker used forced termination'
        })
        if (state.State.Running) throw new Error('A is still running after stop')
        if (state.State.OOMKilled)
            throw new Error('A was OOM-killed; this is not an admitted SIGTERM observation')
        await this.#guard()
        const read = await this.#invoke('b', { op: 'findDocs', collection: 'records', query: {} })
        this.assessment = SmbObservationEvidence.assess(writes, read)
        await this.journal.record({ type: 'post-stop-assessment', assessment: this.assessment })
        if (this.assessment.status !== 'survived' || this.assessment.payloadStatus !== 'matched')
            await this.#diagnostics('pre-fresh')
        if (this.commands.uncertain)
            throw new Error('Uncertain operation; fresh mutation is prohibited')
        await this.#guard()
        const fresh = await this.#invoke('b', {
            op: 'putData',
            collection: 'records',
            id: '4VRNF52JS00',
            data: { name: 'after' }
        })
        const freshRead = await this.#invoke('b', {
            op: 'getDoc',
            collection: 'records',
            id: '4VRNF52JS00'
        })
        this.freshSucceeded =
            SmbObservationEvidence.acknowledged(fresh.request, fresh.transport) &&
            freshRead.decoded.complete &&
            freshRead.decoded.response?.outcome?.ok === true &&
            freshRead.decoded.response.outcome.op === 'getDoc' &&
            isDeepStrictEqual(freshRead.decoded.response.outcome.result?.['4VRNF52JS00'], {
                name: 'after'
            })
        await this.journal.record({
            type: 'fresh-observation',
            fresh,
            freshRead,
            succeeded: this.freshSucceeded
        })
    }

    #resource(role) {
        const resource = this.resources.find((item) => item.role === role)
        if (!resource) throw new Error(`Owned resource missing: ${role}`)
        return resource
    }

    async #inspect(role, exportPhase = false) {
        const resource = this.#resource(role)
        const inspect = JSON.parse(
            (await this.#docker([resource.kind, 'inspect', resource.id], { exportPhase })).stdout
        )[0]
        const labels = inspect.Config?.Labels ?? inspect.Labels
        if (
            inspect.Id !== resource.id ||
            Object.entries(this.labels).some(([name, value]) => labels?.[name] !== value)
        )
            throw new Error('Resource identity/owner changed; retain and stop')
        if (
            resource.kind === 'container' &&
            inspect.Image !==
                (role === 'azurite'
                    ? this.config.artifacts.azuriteImage
                    : this.config.artifacts.clientImage)
        )
            throw new Error('Owned container image identity changed')
        return inspect
    }

    async #diagnostics(phase) {
        await this.journal.record({
            type: 'diagnostic-checkpoint',
            phase,
            resources: this.resources
        })
        for (const resource of this.resources.filter((item) => item.kind === 'container')) {
            const inspect = await this.#inspect(resource.role, true)
            await this.journal.record({ type: 'container-state', phase, resource, inspect })
            await this.#docker(['cp', `${resource.id}:/var/log/observation.log`, '-'], {
                exportPhase: true
            })
        }
        const server = this.resources.find((resource) => resource.role === 'smb')
        if (server && (await this.#inspect('smb', true)).State.Running) {
            await this.#docker(
                ['exec', server.id, 'find', this.shareRoot, '-xdev', '-printf', '%y %m %s %p\n'],
                { exportPhase: true }
            )
            await this.#docker(
                [
                    'exec',
                    server.id,
                    'tar',
                    '-czf',
                    '-',
                    '-C',
                    '/share',
                    `observation/${this.config.runId}`
                ],
                { exportPhase: true }
            )
        }
    }

    #successful() {
        return (
            this.assessment?.status === 'survived' &&
            this.assessment?.payloadStatus === 'matched' &&
            this.freshSucceeded === true
        )
    }

    async #cleanup() {
        for (const resource of [...this.resources].reverse()) {
            await this.#inspect(resource.role, true)
            const args =
                resource.kind === 'container'
                    ? ['container', 'rm', '--force', resource.id]
                    : ['network', 'rm', resource.id]
            await this.#docker(args, { exportPhase: true })
        }
        await this.journal.record({
            type: 'owned-cleanup-complete',
            observedAt: new Date().toISOString()
        })
    }

    #error(error) {
        if (error instanceof Error)
            return { name: error.name, message: error.message, stack: error.stack }
        return { value: String(error), type: typeof error }
    }
}
