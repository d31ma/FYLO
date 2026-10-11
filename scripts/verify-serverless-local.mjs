// Local Cloud Run and Azure Functions qualification lanes, no cloud account
// needed. The sibling of verify-efs-local.mjs.
//
// Each lane mounts the shared storage the way the platform does, then runs the
// EFS-profile crash suite on it, the two-instance root lease drill, and the
// HTTP invocation adapter as two service instances:
//
//   cloudrun-nfs     Filestore as Cloud Run mounts it: NFSv3, no-lock mode.
//   cloudrun-gcs     Cloud Storage volume: gcsfuse against fake-gcs-server.
//   azure-smb-nobrl  Azure Files over SMB 3.1.1 with local-only locks (nobrl),
//                    served by Samba, behind the Functions host (Core Tools)
//                    with Azurite as AzureWebJobsStorage.
//   azure-smb        The same with byte-range locks sent to the server.
//   azure-nfs        Azure Files over NFS 4.1 behind the same Functions host.
//
// What this cannot tell: fake-gcs-server has no objects.move, so gcsfuse
// renames fall back to copy and delete here, while real Cloud Run renames
// with objects.move; Samba on FAT refuses hard links as Azure Files does, but
// the errno and the mount options Azure applies may differ; the instances are privileged containers that mount for themselves,
// where the platforms mount for them; gen1 (gVisor) is not covered because
// Cloud Run volume mounts need gen2. Real latency still needs a live run.
//
//   bun scripts/verify-serverless-local.mjs --platform cloudrun|azure
//       [--mount NAME] [--concurrency 4] [--cache-dir DIR] [--skip-crash]
//
// Needs Docker with privileged containers and the nfsd, nfs and cifs kernel
// modules loaded on the Docker host. Prints a check matrix and exits nonzero
// when any check fails; it does not stop at the first failure, because which
// checks fail on which mount is the result.
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { parseArgs } from 'node:util'

const { values: options } = parseArgs({
    options: {
        platform: { type: 'string' },
        mount: { type: 'string' },
        concurrency: { type: 'string', default: '4' },
        'skip-crash': { type: 'boolean', default: false },
        // Host directory for the Linux build cache; defaults to Docker volumes.
        'cache-dir': { type: 'string' }
    }
})
const repo = process.cwd()
const toolchain = (await readFile('rust-toolchain.toml', 'utf8')).match(
    /channel\s*=\s*"([^"]+)"/
)[1]
const bunVersion = (await readFile('.bun-version', 'utf8')).trim()
// ponytail: pinned images and tools; bump deliberately.
const BASE = 'amazonlinux:2023'
const FAKE_GCS = 'fsouza/fake-gcs-server:1.56.1'
const AZURITE = 'mcr.microsoft.com/azure-storage/azurite:3.37.0'
const GCSFUSE = '3.12.0'
const CORE_TOOLS = '4.15.2'
// Azurite's published development account key, not a secret.
const AZURITE_KEY =
    'Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=='
const NAME = 'fylo-serverless'
const network = NAME
const binaries = '/target/debug'
const local = '/opt/fylo'
const install =
    `mkdir -p ${local} && cp ${binaries}/fylo-rust ${binaries}/fylo-write-preview ` +
    `${binaries}/examples/http_invocation ${local}/`
const cache = options['cache-dir']
    ? { target: join(options['cache-dir'], 'target'), cargo: join(options['cache-dir'], 'cargo') }
    : { target: `${NAME}-target`, cargo: `${NAME}-cargo` }
if (options['cache-dir']) await mkdir(cache.target, { recursive: true })

const smb = (locks) =>
    `mount -t cifs //${NAME}-smb/state /mnt/state -o vers=3.1.1,username=fylo,password=fylo,` +
    `nounix,mfsymlinks,cache=strict,actimeo=30,nosharesock,uid=0,gid=0,file_mode=0600,dir_mode=0755${locks}`
const MOUNTS = {
    'cloudrun-nfs': {
        platform: 'cloudrun',
        server: startNfsServer,
        mount: `mount -t nfs -o vers=3,nolock,proto=tcp,mountproto=tcp ${NAME}-nfsd:/export /mnt/state`
    },
    'cloudrun-gcs': {
        platform: 'cloudrun',
        server: startFakeGcs,
        // Cloud Run turns on implicit directories; the 60 s metadata cache is
        // the gcsfuse default it keeps. FYLO refuses gcsfuse's default 0644
        // files, so the volume needs the file-mode and dir-mode mount options.
        mount:
            `gcsfuse --custom-endpoint http://${NAME}-gcs:4443/storage/v1/ --anonymous-access ` +
            '--implicit-dirs --file-mode 600 --dir-mode 755 --enable-atomic-rename-object=false fylo /mnt/state'
    },
    'azure-smb-nobrl': { platform: 'azure', server: startSamba, mount: smb(',nobrl') },
    'azure-smb': { platform: 'azure', server: startSamba, mount: smb('') },
    // Azure Files over NFS 4.1, on the Premium and Dedicated plans and
    // Container Apps; Azure documents nothing about its lock support.
    'azure-nfs': {
        platform: 'azure',
        server: startNfsServer,
        mount: `mount -t nfs4 -o vers=4.1,proto=tcp ${NAME}-nfsd:/ /mnt/state`
    }
}
const selected = Object.entries(MOUNTS).filter(
    ([name, mount]) =>
        mount.platform === options.platform && (!options.mount || options.mount === name)
)
if (selected.length === 0) {
    throw new Error('--platform must be cloudrun or azure, and --mount one of its mounts')
}

const builder = `FROM ${BASE}
RUN dnf -y -q install gcc gcc-c++ make tar gzip findutils which && dnf clean all
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain ${toolchain}
ENV PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/target CARGO_PROFILE_DEV_DEBUG=0`
// One image plays every role: storage servers, test clients and instances.
const client = `FROM ${BASE}
RUN dnf -y -q install nfs-utils cifs-utils samba dosfstools fuse fuse3 libicu procps-ng findutils unzip tar gzip && dnf clean all
RUN curl -fsSL https://bun.sh/install | bash -s bun-v${bunVersion} >/dev/null && ln -s /root/.bun/bin/bun /usr/local/bin/bun
RUN curl -fsSL -o /tmp/gcsfuse.rpm https://github.com/GoogleCloudPlatform/gcsfuse/releases/download/v${GCSFUSE}/gcsfuse-${GCSFUSE}-1.$(uname -m).rpm && rpm -i --nosignature /tmp/gcsfuse.rpm && rm /tmp/gcsfuse.rpm
RUN arch=$(uname -m | sed 's/aarch64/arm64/;s/x86_64/x64/') && curl -fsSL -o /tmp/func.zip https://github.com/Azure/azure-functions-core-tools/releases/download/${CORE_TOOLS}/Azure.Functions.Cli.linux-$arch.${CORE_TOOLS}.zip && unzip -q /tmp/func.zip -d /opt/func && rm /tmp/func.zip && chmod +x /opt/func/func && ln -s /opt/func/func /usr/local/bin/func`

const results = []
const started = Date.now()
await cleanUp()
try {
    await step('Build helper images', async () => {
        await image(`${NAME}-builder`, builder)
        await image(`${NAME}-client`, client)
    })
    await step('Build Linux binaries', () =>
        docker([
            'run',
            '--rm',
            ...volumes(),
            `${NAME}-builder`,
            'sh',
            '-c',
            'cp -a /src /work && cd /work && ' +
                'cargo build --locked -p fylo-cli --bin fylo-rust --bin fylo-write-preview && ' +
                'cargo build --locked -p fylo-machine --example http_invocation'
        ])
    )
    await docker(['network', 'create', network], { check: false, quiet: true })
    for (const [name, mount] of selected) {
        await step(`Start storage for ${name}`, mount.server)
        if (!options['skip-crash']) await check(name, 'crash suite', () => crashSuite(mount.mount))
        await check(name, 'root lease across instances', () => crossClientDrill(mount.mount))
        await check(name, `${mount.platform} service instances`, () => serviceDrill(name, mount))
        await stopStorage()
    }
} finally {
    await cleanUp()
}
console.log(`\n${'mount'.padEnd(18)}${'check'.padEnd(34)}result`)
for (const { mount, label, error } of results) {
    console.log(`${mount.padEnd(18)}${label.padEnd(34)}${error ? `FAIL ${error}` : 'pass'}`)
}
console.log(`Local serverless lane finished in ${Math.round((Date.now() - started) / 1000)} s`)
if (results.some((result) => result.error)) process.exit(1)

async function check(mount, label, action) {
    try {
        await step(`${mount}: ${label}`, action)
        results.push({ mount, label })
    } catch (error) {
        const message = String(error.message ?? error)
        console.log(`   FAIL ${message}`)
        results.push({ mount, label, error: message.split('\n')[0].slice(0, 900) })
    } finally {
        await removeClients()
    }
}

async function removeClients() {
    // Clients first: a hard NFS mount whose server is gone cannot be torn down.
    for (const container of ['a', 'b', 'crash']) {
        await docker(['rm', '-f', `${NAME}-${container}`], { check: false, quiet: true })
    }
}

async function stopStorage() {
    await removeClients()
    await docker(['exec', `${NAME}-nfsd`, 'sh', '-c', 'exportfs -ua; rpc.nfsd 0'], {
        check: false,
        quiet: true
    })
    for (const server of ['nfsd', 'gcs', 'smb', 'azurite']) {
        await docker(['rm', '-f', `${NAME}-${server}`], { check: false, quiet: true })
    }
}

async function cleanUp() {
    await stopStorage()
    await docker(['network', 'rm', network], { check: false, quiet: true })
}

function volumes() {
    return [
        '-v',
        `${repo}:/src:ro`,
        '-v',
        `${cache.target}:/target`,
        '-v',
        `${cache.cargo}:/root/.cargo/registry`
    ]
}

/** A privileged client that installs the binaries and mounts the share. */
function clientArgs(name, extra = []) {
    return [
        'run',
        '-d',
        '--name',
        `${NAME}-${name}`,
        '--hostname',
        name,
        '--network',
        network,
        '--privileged',
        ...volumes(),
        ...extra,
        `${NAME}-client`
    ]
}

async function startClient(name, mount, setup) {
    await docker(
        [
            ...clientArgs(name),
            'sh',
            '-c',
            `${install} && mkdir -p /mnt/state && ${mount} && ${setup} && echo READY && sleep infinity`
        ],
        { quiet: true }
    )
    await ready(name)
}

async function ready(container, marker = 'READY', seconds = 120) {
    for (let attempt = 0; attempt < seconds; attempt++) {
        const { stdout, stderr } = await docker(['logs', `${NAME}-${container}`], {
            check: false,
            quiet: true
        })
        if ((stdout + stderr).includes(marker)) return
        const { stdout: state } = await docker(
            ['inspect', '-f', '{{.State.Running}}', `${NAME}-${container}`],
            { check: false, quiet: true }
        )
        if (state.trim() === 'false') {
            throw new Error(`${container} exited: ${(stdout + stderr).trim().slice(-600)}`)
        }
        await Bun.sleep(1000)
    }
    throw new Error(`${container} did not become ready`)
}

async function startNfsServer() {
    // NFSv3 for Filestore, which Cloud Run mounts without NLM locking, and
    // NFS 4.1 for Azure Files.
    await docker(
        [
            'run',
            '-d',
            '--name',
            `${NAME}-nfsd`,
            '--network',
            network,
            '--privileged',
            '--tmpfs',
            '/export:size=4g',
            `${NAME}-client`,
            'sh',
            '-c',
            'chmod 777 /export && echo "/export *(rw,sync,no_subtree_check,no_root_squash,insecure,fsid=0)" > /etc/exports && ' +
                'mount -t nfsd nfsd /proc/fs/nfsd && rpcbind && rpc.nfsd 8 2>/dev/null; ' +
                'exportfs -ra && rpc.mountd && echo READY && sleep infinity'
        ],
        { quiet: true }
    )
    await ready('nfsd')
}

async function startFakeGcs() {
    await docker(
        [
            'run',
            '-d',
            '--name',
            `${NAME}-gcs`,
            '--network',
            network,
            FAKE_GCS,
            '-scheme',
            'http',
            '-port',
            '4443',
            '-public-host',
            `${NAME}-gcs:4443`,
            '-backend',
            'memory'
        ],
        { quiet: true }
    )
    // The bucket is created from a client, which also proves the endpoint is up.
    await docker(
        [
            'run',
            '--rm',
            '--network',
            network,
            `${NAME}-client`,
            'sh',
            '-c',
            'for i in $(seq 60); do curl -sf -X POST -H "Content-Type: application/json" ' +
                `-d '{"name":"fylo"}' 'http://${NAME}-gcs:4443/storage/v1/b?project=fylo' && exit 0; sleep 1; done; exit 1`
        ],
        { quiet: true }
    )
}

async function startSamba() {
    // Azure Files has no hard links, extended attributes or Unix extensions,
    // and is case-insensitive. Samba on ext4 or tmpfs would allow links and
    // pass what Azure refuses, so the share sits on FAT, which has none.
    const config = [
        '[global]',
        'server min protocol = SMB3',
        'ea support = no',
        'unix extensions = no',
        'store dos attributes = no',
        'map archive = no',
        '[state]',
        'path = /share',
        'read only = no',
        'force user = root'
    ].join('\n')
    await docker(
        [
            'run',
            '-d',
            '--name',
            `${NAME}-smb`,
            '--network',
            network,
            '--privileged',
            '--tmpfs',
            '/disk:size=4g',
            `${NAME}-client`,
            'sh',
            '-c',
            `printf '%s\\n' '${config.replaceAll('\n', "' '")}' > /etc/samba/smb.conf && ` +
                'truncate -s 3G /disk/share.img && mkfs.vfat -F 32 /disk/share.img >/dev/null && ' +
                'mkdir -p /share && mount -o loop,uid=0,gid=0,fmask=0077,dmask=0022 /disk/share.img /share && ' +
                'useradd -M fylo && (echo fylo; echo fylo) | smbpasswd -a -s fylo >/dev/null && ' +
                'echo READY && exec smbd --foreground --no-process-group --debug-stdout'
        ],
        { quiet: true }
    )
    await ready('smb')
    await Bun.sleep(2000)
}

function crashSuite(mount) {
    return docker([
        'run',
        '--rm',
        '--name',
        `${NAME}-crash`,
        '--network',
        network,
        '--privileged',
        ...volumes(),
        `${NAME}-client`,
        'sh',
        '-c',
        `${install} && mkdir -p /mnt/state && ${mount} && ` +
            'mkdir -p /mnt/state/crash && cd /src && TMPDIR=/mnt/state/crash bun scripts/verify-rust-crash-matrix.mjs ' +
            `--skip-build --binary-dir ${local} --filesystem-profile efs-sidecar-v1 ` +
            `--concurrency ${Number(options.concurrency)} --request-timeout-ms 180000`
    ])
}

/**
 * Two instances on one share: the second must be refused while the first
 * holds the root, see the first's writes after it closes, and take over at
 * once after the holder is killed with SIGKILL.
 */
async function crossClientDrill(mount) {
    for (const client of ['a', 'b'])
        await startClient(client, mount, 'mkdir -p /mnt/state/drill/root')
    const machine = (client, requests) =>
        docker(
            [
                'exec',
                '-i',
                `${NAME}-${client}`,
                `${local}/fylo-rust`,
                'exec',
                '--loop',
                '--root',
                '/mnt/state/drill/root',
                '--exclusive-root',
                '--filesystem-profile',
                'efs-sidecar-v1'
            ],
            {
                input: requests.map((request) => JSON.stringify(request) + '\n').join(''),
                quiet: true
            }
        ).then(({ stdout }) =>
            stdout
                .trim()
                .split('\n')
                .filter(Boolean)
                .map((line) => JSON.parse(line))
        )
    const owner = { uid: 0, groups: [0] }

    const holder = docker(
        [
            'exec',
            `${NAME}-a`,
            'sh',
            '-c',
            `(echo '{"op":"handshake"}'; sleep 600) | ${local}/fylo-rust exec --loop --root /mnt/state/drill/root ` +
                '--exclusive-root --filesystem-profile efs-sidecar-v1'
        ],
        { check: false, quiet: true }
    )
    await Bun.sleep(3000)
    const [refused] = await machine('b', [{ op: 'handshake' }])
    await docker(['exec', `${NAME}-a`, 'pkill', '-KILL', '-f', 'fylo-rust exec'], {
        check: false,
        quiet: true
    })
    const { stdout: held } = await holder
    assert(
        refused?.ok === false && refused.error?.code === 'EROOTLOCKED',
        `a second instance was not refused with EROOTLOCKED while the first held the root ` +
            `(second: ${brief(refused)}, holder: ${brief(held)})`
    )

    const written = await machine('b', [
        { op: 'createCollection', collection: 'records', kind: 'document' },
        {
            op: 'putData',
            collection: 'records',
            id: '4VRNF52JPCO',
            data: { name: 'Ada' },
            access: { mode: 0o640 }
        }
    ])
    assert(
        written.length === 2 && written.every((response) => response.ok),
        `the surviving instance could not take over after SIGKILL: ${JSON.stringify(written)}`
    )
    const [read] = await machine('a', [
        { op: 'getDoc', collection: 'records', id: '4VRNF52JPCO', access: owner }
    ])
    assert(
        read?.ok && read.result?.['4VRNF52JPCO']?.name === 'Ada',
        `a write was not visible to the other instance after close: ${JSON.stringify(read)}`
    )
}

/**
 * The HTTP invocation adapter as two service instances on one share, under
 * the platform's contract: requests on both, concurrent writes split across
 * them, a retried create, and SIGTERM with the platform's grace period.
 */
async function serviceDrill(name, mount) {
    const root = '/mnt/state/service/root'
    const setup = `${install} && mkdir -p /mnt/state && ${mount.mount} && mkdir -p ${root}`
    const urls = []
    for (const [index, instance] of ['a', 'b'].entries()) {
        if (mount.platform === 'cloudrun') {
            // Cloud Run: PORT, K_* variables, a memory limit that also holds
            // the in-memory filesystem, and SIGTERM then SIGKILL after 10 s.
            await docker(
                [
                    ...clientArgs(instance, [
                        '--memory',
                        '512m',
                        '--cpus',
                        '1',
                        '--tmpfs',
                        '/tmp',
                        '-e',
                        'PORT=8080',
                        '-e',
                        `K_SERVICE=${NAME}`,
                        '-e',
                        `K_REVISION=${NAME}-00001-abc`,
                        '-e',
                        `K_CONFIGURATION=${NAME}`,
                        '-e',
                        `FYLO_ROOT=${root}`
                    ]),
                    'sh',
                    '-c',
                    `${setup} && exec ${local}/http_invocation`
                ],
                { quiet: true }
            )
            await ready(instance, 'listening')
            urls.push([instance, 'http://localhost:8080/'])
        } else {
            if (index === 0) await startAzurite()
            const app = await functionApp(name)
            await docker(
                [
                    ...clientArgs(instance, [
                        '-v',
                        `${app}:/app:ro`,
                        '-w',
                        '/app',
                        '-e',
                        'FUNCTIONS_WORKER_RUNTIME=custom',
                        '-e',
                        'FUNCTIONS_CORE_TOOLS_TELEMETRY_OPTOUT=1',
                        '-e',
                        `AzureWebJobsStorage=${azuriteConnection()}`,
                        '-e',
                        `FYLO_ROOT=${root}`
                    ]),
                    'sh',
                    '-c',
                    `${setup} && exec func start --port 7071`
                ],
                { quiet: true }
            )
            await ready(instance, 'Host lock lease acquired', 180)
            urls.push([instance, 'http://localhost:7071/api/fylo'])
        }
    }
    // Requests go in through each instance's loopback: Core Tools binds the
    // host to localhost only.
    const invoke = ([instance, url], request) =>
        docker(
            [
                'exec',
                '-i',
                `${NAME}-${instance}`,
                'curl',
                '-s',
                '-m',
                '60',
                '-X',
                'POST',
                '--data-binary',
                '@-',
                url
            ],
            { input: JSON.stringify(request), quiet: true }
        ).then(({ stdout }) => {
            try {
                return JSON.parse(stdout)
            } catch {
                return { body: stdout.slice(0, 300) }
            }
        })
    const ok = (response) => response?.outcome?.ok === true
    const [a, b] = urls

    for (const request of [
        { op: 'handshake' },
        { op: 'createCollection', collection: 'records', kind: 'document' },
        { op: 'putData', collection: 'records', id: '4VRNF52JPCO', data: { name: 'Ada' } }
    ]) {
        const response = await invoke(a, request)
        assert(ok(response), `${request.op} failed: ${JSON.stringify(response)}`)
    }
    const read = await invoke(b, { op: 'getDoc', collection: 'records', id: '4VRNF52JPCO' })
    assert(
        read?.outcome?.result?.['4VRNF52JPCO']?.name === 'Ada',
        `instance b did not read instance a's write: ${JSON.stringify(read)}`
    )
    console.log(`   warm getDoc ${read.elapsedMs} ms, peak RSS ${read.peakRssKb} kB`)

    // Writes must all commit and survive one at a time across instances, at
    // once on one instance and at once across both. Every phase runs, so the
    // matrix shows which pattern breaks.
    const problems = []
    const expect = (condition, message) => condition || problems.push(message)
    const findAll = { op: 'findDocs', collection: 'records', query: {} }
    for (const [phase, label, targets, together] of [
        ['T', 'one at a time across instances', [a, b, a, b, a, b], false],
        ['U', 'at once on one instance', [a, a, a, a, a, a], true],
        ['V', 'at once across instances', [a, b, a, b, a, b, a, b], true]
    ]) {
        const ids = targets.map((_, index) => `4VRNF52J${phase}${index}0`)
        const put = (id, index) =>
            invoke(targets[index], { op: 'putData', collection: 'records', id, data: { name: id } })
        const responses = []
        if (together) responses.push(...(await Promise.all(ids.map(put))))
        else for (const [index, id] of ids.entries()) responses.push(await put(id, index))
        const failed = responses.filter((response) => !ok(response))
        const listed = JSON.stringify((await invoke(b, findAll))?.outcome?.result ?? '')
        const lost = ids.filter((id, index) => ok(responses[index]) && !listed.includes(id))
        expect(
            failed.length === 0,
            `writes ${label}: ${failed.length}/${ids.length} failed, first ${brief(failed[0]?.outcome ?? failed[0])}`
        )
        expect(lost.length === 0, `writes ${label}: committed but missing ${lost.join(', ')}`)
    }

    const retried = await invoke(b, {
        op: 'putData',
        collection: 'records',
        id: '4VRNF52JPCO',
        data: { name: 'Ada' }
    })
    expect(
        retried?.outcome?.ok === false && /already exists/.test(retried.outcome?.error?.message),
        `a retried create was not refused as existing: ${brief(retried?.outcome ?? retried)}`
    )

    // SIGTERM mid-burst: every acknowledged write survives and b keeps going.
    const burst = Array.from({ length: 12 }, (_, index) => `4VRNF52JR${index.toString(16)}0`)
    const pending = burst.map((id) =>
        invoke(a, { op: 'putData', collection: 'records', id, data: { name: id } }).catch(
            (error) => ({ dropped: String(error) })
        )
    )
    await Bun.sleep(50)
    await docker(['stop', '-t', '10', `${NAME}-a`], { quiet: true })
    const outcomes = await Promise.all(pending)
    const committed = burst.filter((_, index) => ok(outcomes[index]))
    const survivors = await invoke(b, findAll)
    const kept = JSON.stringify(survivors?.outcome?.result ?? '')
    const vanished = committed.filter((id) => !kept.includes(id))
    expect(
        ok(survivors) && vanished.length === 0,
        `acknowledged writes vanished after SIGTERM: ${vanished.join(', ') || brief(survivors?.outcome ?? survivors)}`
    )
    const fresh = await invoke(b, {
        op: 'putData',
        collection: 'records',
        id: '4VRNF52JS00',
        data: { name: 'after' }
    })
    expect(
        ok(fresh),
        `instance b could not write after a was stopped: ${brief(fresh?.outcome ?? fresh)}`
    )
    console.log(`   SIGTERM burst: ${committed.length}/${burst.length} acknowledged before stop`)
    assert(problems.length === 0, problems.join('; '))
}

async function startAzurite() {
    await docker(
        [
            'run',
            '-d',
            '--name',
            `${NAME}-azurite`,
            '--network',
            network,
            AZURITE,
            'azurite',
            '--blobHost',
            '0.0.0.0',
            '--queueHost',
            '0.0.0.0',
            '--tableHost',
            '0.0.0.0',
            '--skipApiVersionCheck'
        ],
        { quiet: true }
    )
}

function azuriteConnection() {
    const host = `http://${NAME}-azurite`
    return (
        `DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey=${AZURITE_KEY};` +
        `BlobEndpoint=${host}:10000/devstoreaccount1;QueueEndpoint=${host}:10001/devstoreaccount1;` +
        `TableEndpoint=${host}:10002/devstoreaccount1;`
    )
}

/** A custom-handler function app that forwards raw HTTP to the adapter. */
async function functionApp(name) {
    const app = await mkdtemp(join(tmpdir(), `${NAME}-${name}-`))
    await mkdir(join(app, 'fylo'))
    await writeFile(
        join(app, 'host.json'),
        JSON.stringify({
            version: '2.0',
            customHandler: {
                description: { defaultExecutablePath: `${local}/http_invocation` },
                enableForwardingHttpRequest: true
            }
        })
    )
    await writeFile(
        join(app, 'fylo', 'function.json'),
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
    process.on('exit', () => Bun.spawnSync(['rm', '-rf', app]))
    return app
}

async function image(tag, dockerfile) {
    await docker(['build', '-q', '-t', tag, '-'], {
        input: dockerfile,
        quiet: true,
        env: { DOCKER_BUILDKIT: '0' }
    })
}

async function step(label, action) {
    const begun = Date.now()
    console.log(`== ${label}`)
    await action()
    console.log(`   ok (${Math.round((Date.now() - begun) / 1000)} s)`)
}

function docker(args, options = {}) {
    return required(['docker', ...args], options)
}

async function required(command, { check = true, quiet = false, input, env } = {}) {
    const subprocess = Bun.spawn(command, {
        env: { ...process.env, ...env },
        stdin: input === undefined ? 'ignore' : new Blob([input]),
        stdout: quiet ? 'pipe' : 'inherit',
        stderr: quiet ? 'pipe' : 'inherit'
    })
    const [stdout, stderr, exitCode] = await Promise.all([
        quiet ? new Response(subprocess.stdout).text() : '',
        quiet ? new Response(subprocess.stderr).text() : '',
        subprocess.exited
    ])
    if (check && exitCode !== 0) {
        throw new Error(
            `${command.slice(0, 3).join(' ')} failed (${exitCode}): ${stderr.trim().slice(-800)}`
        )
    }
    return { stdout, stderr, exitCode }
}

/** A machine response, or its first output line, without the long handshake listing. */
function brief(response) {
    if (typeof response === 'string') {
        try {
            response = JSON.parse(response.trim().split('\n')[0])
        } catch {
            return JSON.stringify(response.slice(0, 200))
        }
    }
    return response?.ok ? 'ok' : JSON.stringify(response?.error ?? response)
}

function assert(condition, message) {
    if (!condition) throw new Error(message)
}
