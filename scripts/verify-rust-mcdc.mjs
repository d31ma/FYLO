#!/usr/bin/env bun

import { CargoArtifacts } from './cargo-artifacts.mjs'
import { readdir, readFile, rm, mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const manifestPath = path.join(root, 'tests/mcdc/decisions.json')
const outputRoot = CargoArtifacts.path('mcdc')
const proofDirectory = path.join(outputRoot, 'proofs')
const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))

assert(manifest.schemaVersion === 1, 'MC/DC manifest schemaVersion must be 1')
assert(manifest.criterion === 'unique-cause-mcdc', 'MC/DC criterion must be unique-cause-mcdc')
assert(
    typeof manifest.scope === 'string' && manifest.scope.includes('not whole-core'),
    'MC/DC manifest must state its bounded coverage scope'
)
assert(Array.isArray(manifest.decisions) && manifest.decisions.length > 0, 'No MC/DC decisions')

const ids = new Set()
const tests = new Set()
const packages = new Set()
for (const decision of manifest.decisions) {
    assert(/^[a-z0-9._-]+$/.test(decision.id), `Unsafe MC/DC decision ID ${decision.id}`)
    assert(!ids.has(decision.id), `Duplicate MC/DC decision ID ${decision.id}`)
    ids.add(decision.id)
    assert(/^[a-z0-9-]+$/.test(decision.package), `${decision.id}: invalid package`)
    packages.add(decision.package)
    assert(
        typeof decision.test === 'string' && decision.test.includes('mcdc_'),
        `${decision.id}: proof test must have an mcdc_ name`
    )
    assert(!tests.has(decision.test), `${decision.id}: duplicate proof test ${decision.test}`)
    tests.add(decision.test)
    assert(
        Array.isArray(decision.conditions) && decision.conditions.length > 0,
        `${decision.id}: conditions are missing`
    )
    assert(
        new Set(decision.conditions).size === decision.conditions.length &&
            decision.conditions.every((condition) => typeof condition === 'string' && condition),
        `${decision.id}: condition names must be non-empty and unique`
    )
    assert(
        typeof decision.safety === 'string' && decision.safety.length >= 32,
        `${decision.id}: safety rationale is missing`
    )
    const sourcePath = checkedSourcePath(decision.source)
    const source = await readFile(sourcePath, 'utf8')
    assert(
        typeof decision.sourceMarker === 'string' && source.includes(decision.sourceMarker),
        `${decision.id}: source marker is absent from ${decision.source}`
    )
}

await rm(outputRoot, { recursive: true, force: true })
await mkdir(proofDirectory, { recursive: true })

const packageArguments = [...packages].sort().flatMap((name) => ['-p', name])
const result = Bun.spawn(
    [
        'bun',
        './scripts/run-rust.mjs',
        'cargo',
        'test',
        '--locked',
        '--all-features',
        ...packageArguments,
        'mcdc_',
        '--',
        '--test-threads=1'
    ],
    {
        cwd: root,
        env: { ...process.env, FYLO_MCDC_REPORT_DIR: proofDirectory },
        stdout: 'pipe',
        stderr: 'pipe'
    }
)
const [stdout, stderr, exitCode] = await Promise.all([
    new Response(result.stdout).text(),
    new Response(result.stderr).text(),
    result.exited
])
process.stdout.write(stdout)
process.stderr.write(stderr)
assert(exitCode === 0, `MC/DC Rust tests failed with exit code ${exitCode}`)

const executed = new Set(
    [...stdout.matchAll(/^test (.+?mcdc_.+?) \.\.\. ok$/gm)].map((match) => match[1])
)
assertSetEqual(executed, tests, 'compiled MC/DC proof tests')

const proofFiles = (await readdir(proofDirectory)).filter((file) => file.endsWith('.json')).sort()
assertSetEqual(
    new Set(proofFiles),
    new Set([...ids].map((id) => `${id}.json`)),
    'emitted MC/DC proof files'
)

const proofs = []
for (const decision of manifest.decisions) {
    const proof = JSON.parse(
        await readFile(path.join(proofDirectory, `${decision.id}.json`), 'utf8')
    )
    verifyProof(decision, proof)
    proofs.push({
        ...proof,
        package: decision.package,
        source: decision.source,
        test: decision.test,
        safety: decision.safety
    })
}

proofs.sort((left, right) => left.decisionId.localeCompare(right.decisionId))
const report = {
    schemaVersion: 1,
    criterion: manifest.criterion,
    scope: manifest.scope,
    decisionCount: proofs.length,
    conditionCount: proofs.reduce((total, proof) => total + proof.conditionCount, 0),
    caseCount: proofs.reduce((total, proof) => total + proof.caseCount, 0),
    decisions: proofs
}
await writeFile(path.join(outputRoot, 'report.json'), `${JSON.stringify(report, null, 2)}\n`)
console.log(
    `MC/DC qualification passed: ${report.decisionCount} decisions, ${report.conditionCount} conditions, ${report.caseCount} cases.`
)

function verifyProof(decision, proof) {
    assert(proof.schemaVersion === 1, `${decision.id}: proof schemaVersion must be 1`)
    assert(proof.criterion === 'unique-cause-mcdc', `${decision.id}: wrong proof criterion`)
    assert(proof.decisionId === decision.id, `${decision.id}: proof decision ID changed`)
    assert(
        proof.conditionCount === decision.conditions.length,
        `${decision.id}: condition count drift`
    )
    assert(Array.isArray(proof.conditions), `${decision.id}: proof conditions are missing`)
    assert(Array.isArray(proof.cases), `${decision.id}: proof cases are missing`)
    assert(proof.caseCount === proof.cases.length, `${decision.id}: case count drift`)
    assert(proof.caseCount >= proof.conditionCount + 1, `${decision.id}: too few proof cases`)
    assert(
        JSON.stringify(proof.conditions.map(({ name }) => name)) ===
            JSON.stringify(decision.conditions),
        `${decision.id}: proof condition order drifted`
    )
    const cases = new Map(proof.cases.map((entry) => [entry.name, entry]))
    assert(cases.size === proof.cases.length, `${decision.id}: duplicate proof case names`)
    const vectors = new Set(proof.cases.map((entry) => JSON.stringify(entry.conditions)))
    assert(vectors.size === proof.cases.length, `${decision.id}: duplicate proof condition vectors`)
    for (const entry of proof.cases) {
        assert(
            Array.isArray(entry.conditions) && entry.conditions.length === proof.conditionCount,
            `${decision.id}/${entry.name}: invalid condition vector`
        )
        assert(
            entry.conditions.every((value) => typeof value === 'boolean') &&
                typeof entry.outcome === 'boolean',
            `${decision.id}/${entry.name}: proof values must be boolean`
        )
    }
    for (const [conditionIndex, condition] of proof.conditions.entries()) {
        const [leftName, rightName] = condition.independencePair ?? []
        const left = cases.get(leftName)
        const right = cases.get(rightName)
        assert(left && right, `${decision.id}/${condition.name}: independence pair is missing`)
        assert(
            left.outcome !== right.outcome,
            `${decision.id}/${condition.name}: outcome did not change`
        )
        assert(
            left.conditions[conditionIndex] !== right.conditions[conditionIndex],
            `${decision.id}/${condition.name}: condition did not change`
        )
        assert(
            left.conditions.every(
                (value, index) => index === conditionIndex || value === right.conditions[index]
            ),
            `${decision.id}/${condition.name}: another condition changed in the independence pair`
        )
    }
}

function checkedSourcePath(relative) {
    assert(typeof relative === 'string' && relative.length > 0, 'MC/DC source path is missing')
    const resolved = path.resolve(root, relative)
    assert(
        resolved.startsWith(`${path.resolve(root)}${path.sep}`),
        `MC/DC source escapes root: ${relative}`
    )
    return resolved
}

function assertSetEqual(actual, expected, label) {
    const missing = [...expected].filter((value) => !actual.has(value))
    const unexpected = [...actual].filter((value) => !expected.has(value))
    assert(
        missing.length === 0 && unexpected.length === 0,
        `${label} drifted; missing=${JSON.stringify(missing)} unexpected=${JSON.stringify(unexpected)}`
    )
}

function assert(condition, message) {
    if (!condition) throw new Error(message)
}
