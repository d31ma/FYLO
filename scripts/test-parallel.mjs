#!/usr/bin/env node
// Run the test suite as N parallel `bun test` processes (one shard = a subset of
// files). Separate processes isolate FYLO's in-process singletons (Cipher,
// STRICT, schema caches) and the warm ttid/chex subprocesses, so sharding is
// safe where `--concurrent` (single process) is not. Each test file already
// uses an isolated temp root, so files never collide across shards.
//
// Shards: FYLO_TEST_SHARDS env, else min(cpus, 6). Extra args pass through to bun.
import { readdirSync } from 'node:fs'
import { join } from 'node:path'
import { spawn } from 'node:child_process'
import os from 'node:os'

const DIRS = ['tests/integration', 'tests/collection']

/** @param {string} dir @returns {string[]} */
function walk(dir) {
    /** @type {string[]} */
    const out = []
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, entry.name)
        if (entry.isDirectory()) out.push(...walk(p))
        else if (entry.name.endsWith('.test.js')) out.push(p)
    }
    return out
}

const passthrough = process.argv.slice(2)
const files = DIRS.flatMap(walk).sort()

// Every test file uses an isolated temp root, and each shard is its own process
// (isolating FYLO's in-process singletons), so files shard freely.
const shardCount = Math.min(
    Number(process.env.FYLO_TEST_SHARDS) || Math.min(os.cpus().length, 6),
    files.length
)
/** @type {string[][]} */
const shards = Array.from({ length: shardCount }, () => [])
files.forEach((file, i) => shards[i % shardCount].push(file))

/** @param {string[]} shardFiles @param {number} i */
function runShard(shardFiles, i) {
    return new Promise((resolve) => {
        const args = [
            '--env-file=tests/.env.test',
            'test',
            ...shardFiles,
            '--timeout',
            '120000',
            ...passthrough
        ]
        const child = spawn('bun', args, { env: process.env })
        let out = ''
        child.stdout.on('data', (d) => (out += d))
        child.stderr.on('data', (d) => (out += d))
        child.on('exit', (code) => resolve({ i, code: code ?? 1, out, count: shardFiles.length }))
    })
}

const t0 = Date.now()
const results = await Promise.all(shards.map(runShard))
const failed = results.filter((r) => r.code !== 0)

for (const r of failed) {
    console.log(`\n===== shard ${r.i} FAILED (${r.count} files) =====`)
    console.log(r.out.trimEnd())
}
for (const r of results.sort((a, b) => a.i - b.i)) {
    console.log(`shard ${r.i}: ${r.code === 0 ? 'PASS' : 'FAIL'} (${r.count} files)`)
}
const secs = ((Date.now() - t0) / 1000).toFixed(1)
console.log(
    `\n${failed.length ? '❌ FAIL' : '✅ PASS'} — ${shards.length} shards, ${files.length} files, ${secs}s`
)
process.exit(failed.length ? 1 : 0)
