#!/usr/bin/env bun

import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { createWebArtifact } from './web-artifact.mjs'
import { smokeSite } from './web-smoke.mjs'

const SHA256 = /^[a-f0-9]{64}$/
const TERMINAL = new Set(['SUCCEED', 'FAILED', 'CANCELLED'])

async function command(program, args, { allowFailure = false } = {}) {
    const child = Bun.spawn([program, ...args], { stdout: 'pipe', stderr: 'pipe' })
    const [status, stdout, stderr] = await Promise.all([
        child.exited,
        new Response(child.stdout).text(),
        new Response(child.stderr).text()
    ])
    if (status !== 0 && !allowFailure) {
        throw new Error(
            `${program} ${args[0] ?? ''} failed: ${stderr.trim() || `status ${status}`}`
        )
    }
    return { status, stdout, stderr }
}

async function awsJson(args) {
    const result = await command('aws', [...args, '--output', 'json'])
    return JSON.parse(result.stdout)
}

function objectKey(config, siteName, suffix) {
    return `${config.artifactPrefix.replace(/^\/+|\/+$/g, '')}/${siteName}/${suffix}`
}

async function readState(bucket, key) {
    const result = await command('aws', ['s3', 'cp', `s3://${bucket}/${key}`, '-'], {
        allowFailure: true
    })
    if (result.status !== 0) return null
    const state = JSON.parse(result.stdout)
    if (!SHA256.test(state.checksum))
        throw new Error(`Invalid release state at s3://${bucket}/${key}`)
    if (state.previousChecksum !== undefined && !SHA256.test(state.previousChecksum)) {
        throw new Error(`Invalid previous checksum at s3://${bucket}/${key}`)
    }
    return state
}

async function writeState(bucket, key, state) {
    const temporary = await mkdtemp(path.join(os.tmpdir(), 'fylo-web-state-'))
    try {
        const file = path.join(temporary, 'state.json')
        await writeFile(file, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 })
        await command('aws', ['s3', 'cp', file, `s3://${bucket}/${key}`, '--only-show-errors'])
    } finally {
        await rm(temporary, { recursive: true, force: true })
    }
}

async function archiveArtifact(bucket, key, artifact, checksum) {
    const existing = await command(
        'aws',
        ['s3api', 'head-object', '--bucket', bucket, '--key', key, '--output', 'json'],
        { allowFailure: true }
    )
    if (existing.status === 0) {
        const metadata = JSON.parse(existing.stdout).Metadata ?? {}
        if (metadata.sha256 !== checksum) {
            throw new Error(`Refusing checksum collision at s3://${bucket}/${key}`)
        }
        return
    }
    await command('aws', [
        's3',
        'cp',
        artifact,
        `s3://${bucket}/${key}`,
        '--metadata',
        `sha256=${checksum}`,
        '--only-show-errors'
    ])
}

async function downloadArtifact(bucket, key, checksum, destination) {
    await command('aws', ['s3', 'cp', `s3://${bucket}/${key}`, destination, '--only-show-errors'])
    const actual = createHash('sha256')
        .update(Buffer.from(await Bun.file(destination).arrayBuffer()))
        .digest('hex')
    if (actual !== checksum)
        throw new Error(`Archived artifact checksum mismatch: expected ${checksum}, got ${actual}`)
}

async function deployToAmplify(site, artifact) {
    const created = await awsJson([
        'amplify',
        'create-deployment',
        '--app-id',
        site.appId,
        '--branch-name',
        site.branch
    ])
    if (!created.jobId || !created.zipUploadUrl)
        throw new Error('Amplify returned an incomplete deployment')
    const upload = await fetch(created.zipUploadUrl, {
        method: 'PUT',
        body: Bun.file(artifact),
        headers: { 'content-type': 'application/zip' }
    })
    if (!upload.ok) throw new Error(`Amplify artifact upload returned HTTP ${upload.status}`)
    await awsJson([
        'amplify',
        'start-deployment',
        '--app-id',
        site.appId,
        '--branch-name',
        site.branch,
        '--job-id',
        created.jobId
    ])

    const deadline = Date.now() + 30 * 60_000
    while (Date.now() < deadline) {
        const result = await awsJson([
            'amplify',
            'get-job',
            '--app-id',
            site.appId,
            '--branch-name',
            site.branch,
            '--job-id',
            created.jobId
        ])
        const status = result.job?.summary?.status
        if (TERMINAL.has(status)) {
            if (status !== 'SUCCEED')
                throw new Error(`Amplify deployment ${created.jobId} ended with ${status}`)
            return created.jobId
        }
        await Bun.sleep(5_000)
    }
    throw new Error(`Timed out waiting for Amplify deployment ${created.jobId}`)
}

async function deployArchived(config, siteName, site, bucket, checksum, temporary) {
    const key = objectKey(config, siteName, `artifacts/${checksum}.zip`)
    const artifact = path.join(temporary, `${checksum}.zip`)
    await downloadArtifact(bucket, key, checksum, artifact)
    const jobId = await deployToAmplify(site, artifact)
    await smokeSite(site)
    return jobId
}

async function main() {
    const [action, siteName, configPath = 'ops/web-release.json'] = process.argv.slice(2)
    if (!['deploy', 'rollback'].includes(action) || !siteName) {
        throw new Error(
            'Usage: bun scripts/amplify-release.mjs <deploy|rollback> <fylo|fxp> [config]'
        )
    }
    const bucket = process.env.FYLO_WEB_RELEASE_BUCKET
    if (!bucket) throw new Error('FYLO_WEB_RELEASE_BUCKET is required')
    const config = JSON.parse(await readFile(configPath, 'utf8'))
    const site = config.sites?.[siteName]
    if (!site) throw new Error(`Unknown site ${JSON.stringify(siteName)}`)
    const currentKey = objectKey(config, siteName, 'state/current.json')
    const current = await readState(bucket, currentKey)
    const temporary = await mkdtemp(path.join(os.tmpdir(), 'fylo-amplify-release-'))
    try {
        if (action === 'rollback') {
            if (!current?.previousChecksum) {
                throw new Error(
                    `No prior successful ${siteName} artifact is available for rollback`
                )
            }
            const target = current.previousChecksum
            const jobId = await deployArchived(config, siteName, site, bucket, target, temporary)
            await writeState(bucket, currentKey, {
                ...current,
                checksum: target,
                previousChecksum: current.checksum,
                deployedAt: new Date().toISOString(),
                jobId
            })
            console.log(JSON.stringify({ action, site: siteName, checksum: target, jobId }))
            return
        }

        const artifact = await createWebArtifact(path.resolve(site.sourceDir), temporary)
        const artifactKey = objectKey(config, siteName, `artifacts/${artifact.checksum}.zip`)
        await archiveArtifact(bucket, artifactKey, artifact.output, artifact.checksum)
        let jobId
        try {
            jobId = await deployToAmplify(site, artifact.output)
            await smokeSite(site)
        } catch (error) {
            if (current && current.checksum !== artifact.checksum) {
                console.error(
                    `Deployment failed; restoring ${siteName} artifact ${current.checksum}`
                )
                await deployArchived(config, siteName, site, bucket, current.checksum, temporary)
            }
            throw error
        }
        await writeState(bucket, currentKey, {
            checksum: artifact.checksum,
            previousChecksum:
                current && current.checksum !== artifact.checksum
                    ? current.checksum
                    : current?.previousChecksum,
            deployedAt: new Date().toISOString(),
            appId: site.appId,
            branch: site.branch,
            jobId
        })
        console.log(JSON.stringify({ action, site: siteName, checksum: artifact.checksum, jobId }))
    } finally {
        await rm(temporary, { recursive: true, force: true })
    }
}

if (import.meta.main)
    main().catch((error) => {
        console.error(error.message)
        process.exitCode = 1
    })
