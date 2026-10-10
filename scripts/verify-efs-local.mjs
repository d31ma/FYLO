// Local EFS-profile and Lambda qualification lane, no AWS account needed.
//
// EFS is NFS 4.1, so the storage checks run against a real NFS 4.1 server
// (kernel nfsd in a privileged container, 90 s lease like EFS) with Linux
// clients mounting it. The Lambda checks run the invocation rehearsal image in
// Floci, which executes real Lambda containers locally. Floci's EFS is a plain
// Docker volume, so it proves Lambda plumbing only; NFS semantics come from
// the NFS lane. Encrypted transport, access points and real EFS latency still
// need a rare live run.
//
//   bun scripts/verify-efs-local.mjs [--concurrency 8] [--cache-dir DIR] [--skip-lambda]
//
// Needs Docker with privileged containers (Docker Desktop or a Linux host with
// the nfsd module loaded) and, for the Lambda phase, the AWS CLI.
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { parseArgs } from 'node:util'

const { values: options } = parseArgs({
    options: {
        concurrency: { type: 'string', default: '8' },
        // Host directory for the Linux build cache; defaults to Docker volumes,
        // which live on the Docker disk image.
        'cache-dir': { type: 'string' },
        'skip-lambda': { type: 'boolean', default: false }
    }
})
const repo = process.cwd()
const toolchain = (await readFile('rust-toolchain.toml', 'utf8')).match(/channel\s*=\s*"([^"]+)"/)[1]
const bunVersion = (await readFile('.bun-version', 'utf8')).trim()
// ponytail: pinned images; bump deliberately.
const BASE = 'amazonlinux:2023'
const LAMBDA_BASE = 'public.ecr.aws/lambda/provided:al2023'
const FLOCI = 'floci/floci:2.2.0'
const NAME = 'fylo-efs-local'
const network = NAME
const binaries = '/target/debug'
// Every crash case spawns several processes; loading them from the container's
// own disk instead of a shared build mount halves the suite's runtime.
const local = '/opt/fylo'
const install = `mkdir -p ${local} && cp ${binaries}/fylo-rust ${binaries}/fylo-write-preview ${local}/`
const cache = options['cache-dir']
    ? { target: join(options['cache-dir'], 'target'), cargo: join(options['cache-dir'], 'cargo') }
    : { target: `${NAME}-target`, cargo: `${NAME}-cargo` }
if (options['cache-dir']) await mkdir(cache.target, { recursive: true })

const builder = `FROM ${BASE}
RUN dnf -y -q install gcc gcc-c++ make tar gzip findutils which && dnf clean all
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain ${toolchain}
ENV PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/target CARGO_PROFILE_DEV_DEBUG=0`
const client = `FROM ${BASE}
RUN dnf -y -q install nfs-utils procps-ng findutils unzip tar gzip && dnf clean all
RUN curl -fsSL https://bun.sh/install | bash -s bun-v${bunVersion} >/dev/null && ln -s /root/.bun/bin/bun /usr/local/bin/bun`

const started = Date.now()
await cleanUp()
try {
    await step('Build helper images', async () => {
        await image(`${NAME}-builder`, builder)
        await image(`${NAME}-client`, client)
    })
    await step('Build Linux binaries', () =>
        docker(['run', '--rm', ...volumes(), `${NAME}-builder`, 'sh', '-c',
            'cp -a /src /work && cd /work && ' +
            'cargo build --locked -p fylo-cli --bin fylo-rust --bin fylo-write-preview && ' +
            'cargo build --locked -p fylo-machine --example lambda_invocation'])
    )
    await step('Start NFS 4.1 server', startNfsServer)
    await step('EFS-profile crash suite on NFS', () =>
        docker(['run', '--rm', '--network', network, '--privileged', ...volumes(), `${NAME}-client`, 'sh', '-c',
            `${install} && mkdir -p /mnt/nfs && mount -t nfs4 -o vers=4.1 ${NAME}-nfsd:/ /mnt/nfs && ` +
            'mkdir -p /mnt/nfs/crash && cd /src && TMPDIR=/mnt/nfs/crash bun scripts/verify-rust-crash-matrix.mjs ' +
            `--skip-build --binary-dir ${local} --filesystem-profile efs-sidecar-v1 ` +
            `--concurrency ${Number(options.concurrency)} --request-timeout-ms 180000`])
    )
    await step('Cross-client lease, visibility and takeover on NFS', crossClientDrill)
    if (!options['skip-lambda']) await step('Lambda invocation in Floci', lambdaSmoke)
    console.log(`Local EFS lane passed in ${Math.round((Date.now() - started) / 1000)} s`)
} finally {
    await cleanUp()
}

async function cleanUp() {
    // Clients first: a hard NFS mount whose server is gone cannot be torn down.
    for (const container of ['a', 'b', 'floci']) {
        await docker(['rm', '-f', `${NAME}-${container}`], { check: false, quiet: true })
    }
    // Stop the kernel server threads first, or the container cannot exit.
    await docker(['exec', `${NAME}-nfsd`, 'sh', '-c', 'exportfs -ua; rpc.nfsd 0'], { check: false, quiet: true })
    await docker(['rm', '-f', `${NAME}-nfsd`], { check: false, quiet: true })
    await docker(['network', 'rm', network], { check: false, quiet: true })
}

function volumes() {
    return ['-v', `${repo}:/src:ro`, '-v', `${cache.target}:/target`, '-v', `${cache.cargo}:/root/.cargo/registry`]
}

async function startNfsServer() {
    await docker(['network', 'create', network], { check: false, quiet: true })
    // An overlay filesystem cannot be exported; tmpfs can. The tests interrupt
    // clients, not the server, so server-side media durability is not under
    // test, and memory keeps every sync a client issues off the host disk.
    await docker(['run', '-d', '--name', `${NAME}-nfsd`, '--network', network, '--privileged',
        '--tmpfs', '/export:size=4g', `${NAME}-client`, 'sh', '-c',
        'chmod 777 /export && echo "/export *(rw,sync,no_subtree_check,no_root_squash,fsid=0,insecure)" > /etc/exports && ' +
        'mount -t nfsd nfsd /proc/fs/nfsd && rpcbind && rpc.nfsd -N 3 -V 4.1 8 2>/dev/null; ' +
        'exportfs -ra && rpc.mountd -N 3 && echo READY && sleep infinity'], { quiet: true })
    for (let attempt = 0; attempt < 60; attempt++) {
        if ((await docker(['logs', `${NAME}-nfsd`], { quiet: true })).stdout.includes('READY')) return
        await Bun.sleep(1000)
    }
    throw new Error('NFS server did not start')
}

/**
 * Two independent NFS clients: the second is refused while the first holds
 * the root, sees the first's writes after it closes, and takes over at once
 * after the holder is killed with SIGKILL.
 */
async function crossClientDrill() {
    for (const client of ['a', 'b']) {
        await docker(['run', '-d', '--name', `${NAME}-${client}`, '--network', network, '--privileged',
            ...volumes(), `${NAME}-client`, 'sh', '-c',
            `${install} && mkdir -p /mnt/nfs && mount -t nfs4 -o vers=4.1 ${NAME}-nfsd:/ /mnt/nfs && mkdir -p /mnt/nfs/drill/root && sleep infinity`],
        { quiet: true })
    }
    await Bun.sleep(2000)
    const machine = (client, requests) => docker(['exec', '-i', `${NAME}-${client}`,
        `${local}/fylo-rust`, 'exec', '--loop', '--root', '/mnt/nfs/drill/root',
        '--exclusive-root', '--filesystem-profile', 'efs-sidecar-v1'],
    { input: requests.map((request) => JSON.stringify(request) + '\n').join(''), quiet: true })
        .then(({ stdout }) => stdout.trim().split('\n').filter(Boolean).map((line) => JSON.parse(line)))
    const owner = { uid: 0, groups: [0] }

    const holder = docker(['exec', `${NAME}-a`, 'sh', '-c',
        `(echo '{"op":"handshake"}'; sleep 600) | ${local}/fylo-rust exec --loop --root /mnt/nfs/drill/root ` +
        '--exclusive-root --filesystem-profile efs-sidecar-v1'], { check: false, quiet: true })
    await Bun.sleep(3000)
    const [refused] = await machine('b', [{ op: 'handshake' }])
    assert(refused.ok === false && refused.error?.code === 'EROOTLOCKED',
        `a second client was not refused while the root was held: ${JSON.stringify(refused)}`)
    await docker(['exec', `${NAME}-a`, 'pkill', '-KILL', '-f', 'fylo-rust exec'], { quiet: true })
    await holder

    const written = await machine('b', [
        { op: 'createCollection', collection: 'records', kind: 'document' },
        { op: 'putData', collection: 'records', id: '4VRNF52JPCO', data: { name: 'Ada' }, access: { mode: 0o640 } }
    ])
    assert(written.every((response) => response.ok),
        `the surviving client could not take over after SIGKILL: ${JSON.stringify(written)}`)
    const [read] = await machine('a', [
        { op: 'getDoc', collection: 'records', id: '4VRNF52JPCO', access: owner }
    ])
    assert(read.ok && read.result?.['4VRNF52JPCO']?.name === 'Ada',
        `a write was not visible to the other client after close: ${JSON.stringify(read)}`)
}

/** Run the invocation rehearsal as a Lambda function in Floci, with an EFS mount. */
async function lambdaSmoke() {
    const context = await mkdtemp(join(tmpdir(), 'fylo-efs-local-lambda-'))
    const endpoint = 'http://localhost:4566'
    const aws = (...args) => required(['aws', '--endpoint-url', endpoint, '--output', 'json', ...args], {
        quiet: true,
        env: { ...process.env, AWS_ACCESS_KEY_ID: 'test', AWS_SECRET_ACCESS_KEY: 'test', AWS_REGION: 'us-east-1', AWS_PAGER: '' }
    }).then(({ stdout }) => (stdout.trim() ? JSON.parse(stdout) : {}))
    try {
        await docker(['run', '--rm', '-v', `${cache.target}:/target:ro`, '-v', `${context}:/out`, BASE,
            'cp', `${binaries}/examples/lambda_invocation`, '/out/bootstrap'], { quiet: true })
        await writeFile(join(context, 'Dockerfile'),
            `FROM ${LAMBDA_BASE}\nCOPY bootstrap /var/runtime/bootstrap\nCMD ["fylo"]\n`)
        await docker(['build', '-q', '-t', `${NAME}-lambda:latest`, context], { quiet: true, env: { DOCKER_BUILDKIT: '0' } })
        await docker(['run', '-d', '--name', `${NAME}-floci`, '-p', '4566:4566',
            '-v', '/var/run/docker.sock:/var/run/docker.sock', '-u', 'root', FLOCI], { quiet: true })
        for (let attempt = 0; attempt < 60; attempt++) {
            if (await fetch(endpoint).then(() => true, () => false)) break
            await Bun.sleep(500)
        }
        const fileSystem = (await aws('efs', 'create-file-system', '--creation-token', NAME)).FileSystemId
        const subnet = (await aws('ec2', 'describe-subnets')).Subnets[0].SubnetId
        const group = (await aws('ec2', 'describe-security-groups')).SecurityGroups[0].GroupId
        await aws('efs', 'create-mount-target', '--file-system-id', fileSystem, '--subnet-id', subnet, '--security-groups', group)
        const accessPoint = (await aws('efs', 'create-access-point', '--file-system-id', fileSystem,
            '--root-directory', 'Path=/fylo')).AccessPointArn
        await aws('lambda', 'create-function', '--function-name', NAME, '--package-type', 'Image',
            '--code', `ImageUri=${NAME}-lambda:latest`, '--role', 'arn:aws:iam::000000000000:role/fylo',
            '--timeout', '30', '--vpc-config', `SubnetIds=${subnet},SecurityGroupIds=${group}`,
            '--file-system-configs', `Arn=${accessPoint},LocalMountPath=/mnt/state`,
            '--environment', 'Variables={FYLO_ROOT=/mnt/state}')
        const invoke = async (request) => {
            const out = join(context, 'response.json')
            await aws('lambda', 'invoke', '--function-name', NAME, '--cli-binary-format', 'raw-in-base64-out',
                '--payload', JSON.stringify(request), out)
            return JSON.parse(await readFile(out, 'utf8'))
        }
        const responses = []
        for (const request of [
            { op: 'handshake' },
            { op: 'createCollection', collection: 'records', kind: 'document' },
            { op: 'putData', collection: 'records', id: '4VRNF52JPCO', data: { name: 'Ada' } },
            { op: 'getDoc', collection: 'records', id: '4VRNF52JPCO' }
        ]) responses.push(await invoke(request))
        for (const response of responses) {
            assert(response.outcome?.ok === true, `Lambda invocation failed: ${JSON.stringify(response)}`)
        }
        assert(responses.at(-1).outcome.result['4VRNF52JPCO']?.name === 'Ada',
            'a Lambda write was not readable by the next invocation')
        console.log(`Lambda warm getDoc ${responses.at(-1).elapsedMs} ms, peak RSS ${responses.at(-1).peakRssKb} kB`)
    } finally {
        await docker(['rm', '-f', ...(await docker(['ps', '-aq', '--filter', 'name=floci-'], { quiet: true })).stdout.split(/\s+/).filter(Boolean)],
            { check: false, quiet: true })
        await rm(context, { recursive: true, force: true })
    }
}

async function image(tag, dockerfile) {
    await docker(['build', '-q', '-t', tag, '-'], { input: dockerfile, quiet: true, env: { DOCKER_BUILDKIT: '0' } })
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
        throw new Error(`${command.slice(0, 3).join(' ')} failed (${exitCode}): ${stderr.trim().slice(-800)}`)
    }
    return { stdout, stderr, exitCode }
}

function assert(condition, message) {
    if (!condition) throw new Error(message)
}
