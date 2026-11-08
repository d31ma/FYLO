// `.vdbx` byte-range lock qualification on a local NFS 4.1 server (ADR 0016
// section 7, docs/operations/vdbx-byte-range-lock-qualification.md).
//
// The same kernel nfsd in a privileged container as `bun run efs:local`, with
// two Linux clients mounting it. Client a runs the full single-client
// harness; then both clients load one shared file at once, which is the
// cross-client case a second machine would be.
//
//   bun scripts/verify-vdbx-locks-nfs.mjs [--cache-dir DIR] [--seconds 10]
//       [--export tmpfs|ext4] [--repeat 1]
//
// `--export ext4` serves a loop-mounted ext4 image instead of tmpfs, whose
// coarse change attribute can leave another client's page cache stale.
// `--repeat N` runs the two-client load N times and reports every result.
//
// Needs Docker with privileged containers.
import { mkdir, readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { parseArgs } from 'node:util'

const { values: options } = parseArgs({
    options: {
        'cache-dir': { type: 'string' },
        seconds: { type: 'string', default: '10' },
        export: { type: 'string', default: 'tmpfs' },
        repeat: { type: 'string', default: '1' }
    }
})
if (!['tmpfs', 'ext4'].includes(options.export)) throw new Error('--export is tmpfs or ext4')
const repeats = Math.max(1, Number(options.repeat))
const repo = process.cwd()
const toolchain = (await readFile('rust-toolchain.toml', 'utf8')).match(
    /channel\s*=\s*"([^"]+)"/
)[1]
// ponytail: pinned image; bump deliberately.
const BASE = 'amazonlinux:2023'
const NAME = 'fylo-vdbx-locks'
const network = NAME
const harness = '/target/release/fylo-lock-harness'
const local = '/opt/fylo/fylo-lock-harness'
const seconds = String(Number(options.seconds))
const cache = options['cache-dir']
    ? { target: join(options['cache-dir'], 'target'), cargo: join(options['cache-dir'], 'cargo') }
    : { target: `${NAME}-target`, cargo: `${NAME}-cargo` }
if (options['cache-dir']) await mkdir(cache.target, { recursive: true })

const builder = `FROM ${BASE}
RUN dnf -y -q install gcc make tar gzip findutils which && dnf clean all
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain ${toolchain}
ENV PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/target
# What rust-toolchain.toml asks for, so a --rm build does not fetch it each run.
RUN rustup component add clippy rustfmt && rustup target add wasm32-unknown-unknown`
const client = `FROM ${BASE}
RUN dnf -y -q install nfs-utils procps-ng findutils e2fsprogs && dnf clean all`

const started = Date.now()
await cleanUp()
try {
    await step('Build helper images', async () => {
        await image(`${NAME}-builder`, builder)
        await image(`${NAME}-client`, client)
    })
    await step('Build the Linux harness', () =>
        docker([
            'run',
            '--rm',
            ...volumes(),
            `${NAME}-builder`,
            'sh',
            '-c',
            'cp -a /src /work && cd /work && ' +
                'cargo build --locked --release -p fylo-storage-native --bin fylo-lock-harness'
        ])
    )
    await step('Start NFS 4.1 server', startNfsServer)
    await step('Mount it from two clients', startClients)
    await step('Single-client harness on NFS', () =>
        docker([
            'exec',
            `${NAME}-a`,
            local,
            'qualify',
            '--dir',
            '/mnt/nfs/locks',
            '--seconds',
            seconds,
            '--label',
            `NFS 4.1 (local nfsd, ${options.export} export), one client`,
            '--json',
            '/mnt/nfs/locks/single.json'
        ])
    )
    const failed = []
    for (let run = 1; run <= repeats; run++) {
        try {
            await step(`Two clients loading one file (${run} of ${repeats})`, twoClients)
        } catch (error) {
            console.log(`   FAILED: ${error.message.split('\n')[0]}`)
            failed.push(run)
        }
    }
    console.log(
        `Two-client runs on ${options.export}: ${repeats - failed.length} of ${repeats} passed`
    )
    if (failed.length) throw new Error(`two-client runs ${failed.join(', ')} failed`)
    console.log(`NFS lock lane passed in ${Math.round((Date.now() - started) / 1000)} s`)
} finally {
    await cleanUp()
}

async function twoClients() {
    const file = '/mnt/nfs/locks/shared.vdbx'
    await docker(['exec', `${NAME}-a`, local, 'prepare', '--file', file])
    const startAt = String(Math.floor(Date.now() / 1000) + 5)
    await Promise.all(
        ['a', 'b'].map((client, machine) =>
            docker([
                'exec',
                `${NAME}-${client}`,
                local,
                'share',
                '--file',
                file,
                '--machines',
                '2',
                '--machine',
                String(machine),
                '--procs',
                '4',
                '--threads',
                '2',
                '--seconds',
                seconds,
                '--start-at',
                startAt,
                '--sync',
                '--json',
                `/mnt/nfs/locks/${client}.json`
            ])
        )
    )
    await docker([
        'exec',
        `${NAME}-b`,
        local,
        'verify',
        '--file',
        file,
        '/mnt/nfs/locks/a.json',
        '/mnt/nfs/locks/b.json'
    ])
}

async function cleanUp() {
    // Clients first: a hard NFS mount whose server is gone cannot be torn down.
    for (const container of ['a', 'b']) {
        await docker(['rm', '-f', `${NAME}-${container}`], { check: false, quiet: true })
    }
    await docker(
        [
            'exec',
            `${NAME}-nfsd`,
            'sh',
            '-c',
            'exportfs -ua; rpc.nfsd 0; umount /export 2>/dev/null; true'
        ],
        {
            check: false,
            quiet: true
        }
    )
    await docker(['rm', '-f', `${NAME}-nfsd`], { check: false, quiet: true })
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

async function startNfsServer() {
    await docker(['network', 'create', network], { check: false, quiet: true })
    // An overlay filesystem cannot be exported; tmpfs and a loop-mounted ext4
    // image can.
    const exported =
        options.export === 'ext4'
            ? 'truncate -s 256M /export.img && mkfs.ext4 -q /export.img && mkdir -p /export && ' +
              'mount -o loop /export.img /export && '
            : ''
    await docker(
        [
            'run',
            '-d',
            '--name',
            `${NAME}-nfsd`,
            '--network',
            network,
            '--privileged',
            ...(options.export === 'tmpfs' ? ['--tmpfs', '/export:size=256m'] : []),
            `${NAME}-client`,
            'sh',
            '-c',
            exported +
                'chmod 777 /export && echo "/export *(rw,sync,no_subtree_check,no_root_squash,fsid=0,insecure)" > /etc/exports && ' +
                'mount -t nfsd nfsd /proc/fs/nfsd && rpcbind && rpc.nfsd -N 3 -V 4.1 8 2>/dev/null; ' +
                'exportfs -ra && rpc.mountd -N 3 && echo READY && sleep infinity'
        ],
        { quiet: true }
    )
    for (let attempt = 0; attempt < 60; attempt++) {
        if ((await docker(['logs', `${NAME}-nfsd`], { quiet: true })).stdout.includes('READY'))
            return
        await Bun.sleep(1000)
    }
    throw new Error('NFS server did not start')
}

async function startClients() {
    for (const name of ['a', 'b']) {
        await docker(
            [
                'run',
                '-d',
                '--name',
                `${NAME}-${name}`,
                '--network',
                network,
                '--privileged',
                '-v',
                `${cache.target}:/target:ro`,
                `${NAME}-client`,
                'sh',
                '-c',
                `mkdir -p /opt/fylo /mnt/nfs && cp ${harness} ${local} && ` +
                    `mount -t nfs4 -o vers=4.1 ${NAME}-nfsd:/ /mnt/nfs && mkdir -p /mnt/nfs/locks && ` +
                    'echo MOUNTED && sleep infinity'
            ],
            { quiet: true }
        )
    }
    for (const name of ['a', 'b']) {
        for (let attempt = 0; ; attempt++) {
            const { stdout } = await docker(['logs', `${NAME}-${name}`], { quiet: true })
            if (stdout.includes('MOUNTED')) break
            if (attempt >= 60) throw new Error(`client ${name} did not mount NFS`)
            await Bun.sleep(1000)
        }
    }
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
        throw new Error(`${command.join(' ')} exited ${exitCode}\n${stdout}${stderr}`)
    }
    return { stdout, stderr, exitCode }
}
