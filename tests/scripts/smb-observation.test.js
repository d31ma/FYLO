import assert from 'node:assert/strict'
import { test } from 'node:test'
import { SmbObservationConfig } from '../../scripts/smb-observation-config.mjs'
import { SmbObservationEvidence } from '../../scripts/smb-observation-evidence.mjs'
import { SmbObservation } from '../../scripts/smb-observation.mjs'

const first = '4VRNF52JR00'
const second = '4VRNF52JR10'
const third = '4VRNF52JR20'
const root = '/mnt/state/service/root'
const now = Date.parse('2026-10-12T00:00:00.000Z')
const runId = '11111111-1111-4111-8111-111111111111'
const digest = 'a'.repeat(64)

function request(id = first) {
    return { op: 'putData', collection: 'records', root, id, data: { name: id } }
}

function transport(response, overrides = {}) {
    const body = JSON.stringify(response)
    return {
        argv: ['docker', 'exec', 'fixture-a', 'curl'],
        startedAt: '2026-10-12T00:00:00.000Z',
        completedAt: '2026-10-12T00:00:00.010Z',
        exitCode: 0,
        signal: null,
        timedOut: false,
        stdout: `HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n${body}`,
        stderr: '',
        error: null,
        ...overrides
    }
}

function write(id = first, overrides = {}) {
    return {
        request: request(id),
        transport: transport({ outcome: { op: 'putData', ok: true, result: id } }, overrides)
    }
}

function read(result, overrides = {}) {
    return {
        request: { op: 'findDocs', collection: 'records', root, query: {} },
        transport: transport({ outcome: { op: 'findDocs', ok: true, result } }, overrides)
    }
}

function plan(overrides = {}) {
    return {
        owner: 'test-owner',
        sourceRoot: '/source/fylo',
        outputParent: '/evidence',
        hostMachineId: 'a'.repeat(32),
        dockerDaemonId: 'test-daemon-1234567890',
        dockerSocket: '/var/run/docker.sock',
        dockerRootPath: '/var/lib/docker',
        hostCgroup: '/sys/fs/cgroup/fylo-observation',
        admission: {
            owner: 'test-owner',
            approvedAt: new Date(now).toISOString(),
            expiresAt: new Date(now + 600000).toISOString(),
            projectIo: true,
            exclusiveSlot: true,
            healthReady: true,
            healthOwner: '01a12c31-5976-747d-845c-00610048b0cc',
            hostMachineId: 'a'.repeat(32),
            dockerDaemonId: 'test-daemon-1234567890',
            availableMiB: 8192,
            backingDisks: [{ volume: 'F:', freeMiB: 10000, floorMiB: 5000 }]
        },
        artifacts: {
            nodeVersion: 'v23.5.0',
            sourceHead: '1353cc307005c676f1e90fb685f759d1b97da5a6',
            clientImage: `sha256:${digest}`,
            azuriteImage: `sha256:${digest}`,
            provenance: { path: '/binaries/provenance.json', sha256: digest },
            binaries: {
                fylo: { path: '/binaries/fylo-rust', sha256: digest },
                preview: { path: '/binaries/fylo-write-preview', sha256: digest },
                http: { path: '/binaries/http_invocation', sha256: digest }
            },
            sources: {
                'crates/fylo-storage-native/src/write.rs': digest,
                'crates/fylo-storage-native/src/write/version.rs': digest,
                'scripts/verify-serverless-local.mjs': digest,
                'scripts/serverless-outcome-assessment.mjs': digest,
                'scripts/verify-browser-wasm-host.mjs': digest,
                'tests/scripts/serverless-outcome-assessment.test.js': digest,
                'scripts/smb-observation-config.mjs': digest,
                'scripts/smb-observation-evidence.mjs': digest,
                'scripts/smb-observation-process.mjs': digest,
                'scripts/smb-observation-journal.mjs': digest,
                'scripts/smb-observation.mjs': digest,
                'scripts/verify-smb-observation.mjs': digest
            }
        },
        tools: {
            bun: '1.3.11',
            functions: '4.15.2',
            dockerServer: '29.0.0',
            python: '3.12.0',
            containerd: '2.2.0'
        },
        diskFloors: [
            { path: '/evidence', floorMiB: 5000 },
            { path: '/var/lib/docker', floorMiB: 5000 }
        ],
        ...overrides
    }
}

test('mixed write acknowledgments exclude machine failures and dropped transports', () => {
    const acknowledged = write(first)
    const refused = {
        request: request(second),
        transport: transport({ outcome: { ok: false, error: { code: 'EIO', message: 'refused' } } })
    }
    const dropped = write(third, { exitCode: 28, timedOut: true, stderr: 'curl timed out' })
    const assessment = SmbObservationEvidence.assess(
        [acknowledged, refused, dropped],
        read({ [first]: request().data })
    )
    assert.equal(assessment.status, 'survived')
    assert.deepEqual(assessment.acknowledged, [first])
    assert.deepEqual(assessment.missing, [])
    assert.equal(assessment.payloadStatus, 'matched')
})

test('a partial listing reports only its absent acknowledged ID', () => {
    const assessment = SmbObservationEvidence.assess(
        [write(first), write(second)],
        read({ [first]: request().data })
    )
    assert.equal(assessment.status, 'missing')
    assert.deepEqual(assessment.present, [first])
    assert.deepEqual(assessment.missing, [second])
    assert.equal(assessment.payloadStatus, 'incomplete')
})

test('exact key survival does not hide a changed payload', () => {
    const assessment = SmbObservationEvidence.assess(
        [write()],
        read({ [first]: { name: 'changed' } })
    )
    assert.equal(assessment.status, 'survived')
    assert.deepEqual(assessment.mismatchedPayloads, [first])
    assert.equal(assessment.payloadStatus, 'mismatch')
})

test('zero acknowledgments cannot establish durability', () => {
    const assessment = SmbObservationEvidence.assess([write(first, { exitCode: 28 })], read({}))
    assert.equal(assessment.status, 'inconclusive')
    assert.deepEqual(assessment.acknowledged, [])
    assert.deepEqual(assessment.missing, [])
    assert.equal(assessment.payloadStatus, 'inconclusive')
})

test('a nonzero read exit preserves complete raw transport and cannot imply missing writes', () => {
    const observation = read(
        {},
        { exitCode: 28, stderr: 'timeout details\nsecond line\n', timedOut: true }
    )
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
    assert.equal(assessment.transport, observation.transport)
    assert.equal(assessment.decoded.transport.stdout, observation.transport.stdout)
    assert.equal(assessment.decoded.transport.stderr, observation.transport.stderr)
    assert.equal(assessment.decoded.httpStatus, 200)
    assert.deepEqual(assessment.response, { outcome: { op: 'findDocs', ok: true, result: {} } })
})

test('an HTTP failure retains its successful-shaped body without establishing a listing', () => {
    const observation = read({})
    observation.transport.stdout = observation.transport.stdout.replace(
        '200 OK',
        '503 Service Unavailable'
    )
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.decoded.httpStatus, 503)
    assert.equal(assessment.transport.stdout, observation.transport.stdout)
    assert.deepEqual(assessment.missing, [])
})

test('a malformed body is retained in full and leaves survival unknown', () => {
    const observation = read({}, { stdout: 'HTTP/1.1 200 OK\r\n\r\n{"outcome":\nnot-json\n' })
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.decoded.body, '{"outcome":\nnot-json\n')
    assert.equal(assessment.transport.stdout, observation.transport.stdout)
    assert.equal(assessment.decoded.parseError.name, 'SyntaxError')
    assert.deepEqual(assessment.missing, [])
})

test('HTTP 200 with no completed machine response is unknown', () => {
    const observation = read({})
    observation.transport = transport({ outcome: { noCompletedResponse: 'deadline elapsed' } })
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.response, { outcome: { noCompletedResponse: 'deadline elapsed' } })
    assert.deepEqual(assessment.missing, [])
})

test('a transport exception retains its original evidence without establishing a listing', () => {
    const error = { name: 'Error', message: 'spawn failed', stack: 'complete stack' }
    const observation = read({}, { exitCode: null, error, stderr: 'full failure details' })
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.transport.error, error)
    assert.equal(assessment.transport.stderr, 'full failure details')
    assert.deepEqual(assessment.missing, [])
})

test('a signaled transport cannot establish a listing', () => {
    const assessment = SmbObservationEvidence.assess([write()], read({}, { signal: 'SIGTERM' }))
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('an explicitly incomplete capture cannot establish a listing', () => {
    const assessment = SmbObservationEvidence.assess(
        [write()],
        read({}, { evidenceIncomplete: true })
    )
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.transport.evidenceIncomplete, true)
    assert.deepEqual(assessment.missing, [])
})

test('a failed request-input transport cannot establish a listing', () => {
    const inputError = { name: 'Error', message: 'stdin pipe failed' }
    const assessment = SmbObservationEvidence.assess([write()], read({}, { inputError }))
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.transport.inputError, inputError)
    assert.deepEqual(assessment.missing, [])
})

test('interim HTTP headers do not replace the final status or discard raw output', () => {
    const captured = transport({ outcome: { op: 'putData', ok: true, result: first } })
    captured.stdout = `HTTP/1.1 100 Continue\r\n\r\n${captured.stdout}`
    const decoded = SmbObservationEvidence.decode(captured, request())
    assert.equal(decoded.complete, true)
    assert.equal(decoded.httpStatus, 200)
    assert.equal(decoded.transport.stdout, captured.stdout)
    assert.equal(SmbObservationEvidence.acknowledged(request(), captured), true)
})

test('a returned ID different from the request is not an acknowledgment', () => {
    const captured = transport({ outcome: { op: 'putData', ok: true, result: second } })
    assert.equal(SmbObservationEvidence.acknowledged(request(), captured), false)
})

test('a success response without a returned ID is not a correlated acknowledgment', () => {
    const captured = transport({ outcome: { op: 'putData', ok: true } })
    assert.equal(SmbObservationEvidence.acknowledged(request(), captured), false)
})

test('an unidentified result object is not a correlated acknowledgment', () => {
    const captured = transport({ outcome: { op: 'putData', ok: true, result: {} } })
    assert.equal(SmbObservationEvidence.acknowledged(request(), captured), false)
})

test('an absent request ID cannot become an acknowledgment', () => {
    const invalid = request()
    delete invalid.id
    assert.equal(SmbObservationEvidence.acknowledged(invalid, write().transport), false)
})

test('a mismatched response operation is not an acknowledgment', () => {
    const captured = transport({ outcome: { op: 'getDoc', ok: true, result: first } })
    assert.equal(SmbObservationEvidence.acknowledged(request(), captured), false)
})

test('a read of a different collection cannot establish missing acknowledged writes', () => {
    const observation = read({})
    observation.request.collection = 'other'
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.scope, false)
    assert.deepEqual(assessment.missing, [])
})

test('a filtered read cannot establish missing acknowledged writes', () => {
    const observation = read({})
    observation.request.query = { name: 'different' }
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.scope, false)
    assert.deepEqual(assessment.missing, [])
})

test('a matching absent root is invalid scope rather than proof of missing writes', () => {
    const mutation = write()
    const observation = read({})
    delete mutation.request.root
    delete observation.request.root
    const assessment = SmbObservationEvidence.assess([mutation], observation)
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.scope, false)
    assert.deepEqual(assessment.missing, [])
})

test('a read response for another operation cannot establish missing writes', () => {
    const observation = read({})
    observation.transport = transport({ outcome: { op: 'getDoc', ok: true, result: {} } })
    const assessment = SmbObservationEvidence.assess([write()], observation)
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('a configuration generates an isolated namespace instead of trusting a supplied name', () => {
    const config = SmbObservationConfig.create(plan({ namespace: 'fylo-serverless' }), {
        runId,
        now
    })
    assert.equal(config.namespace, `fylo-smb-fat-${runId}`)
    assert.equal(config.backing, 'fat')
})

test('an unsafe owner is rejected before resources can be launched', () => {
    assert.throws(
        () => SmbObservationConfig.create(plan({ owner: 'unsafe;command' }), { runId, now }),
        /owner/i
    )
})

test('a malformed run ID is rejected before resources can be named', () => {
    assert.throws(
        () => SmbObservationConfig.create(plan(), { runId: '-'.repeat(36), now }),
        /UUID/i
    )
})

test('an evidence path within the source is rejected', () => {
    assert.throws(
        () =>
            SmbObservationConfig.create(plan({ outputParent: '/source/fylo/evidence' }), {
                runId,
                now
            }),
        /outside.*source/i
    )
})

test('an expired admission cannot launch a new observation', () => {
    assert.throws(
        () => SmbObservationConfig.create(plan(), { runId, now: now + 660000 }),
        /admission/i
    )
})

test('evidence export completes before cleanup begins', async () => {
    const events = []
    const result = await SmbObservation.finish({
        exportEvidence: async () => {
            events.push('export')
        },
        cleanup: async () => {
            events.push('cleanup')
        },
        retainFixture: false,
        uncertain: false
    })
    assert.deepEqual(events, ['export', 'cleanup'])
    assert.deepEqual(result, { exported: true, retained: false })
})

test('failed export prevents cleanup and preserves the original error', async () => {
    const error = new Error('evidence destination unavailable')
    const events = []
    await assert.rejects(
        SmbObservation.finish({
            exportEvidence: async () => {
                events.push('export')
                throw error
            },
            cleanup: async () => {
                events.push('cleanup')
            },
            retainFixture: false,
            uncertain: false
        }),
        (failure) => failure === error
    )
    assert.deepEqual(events, ['export'])
})

test('uncertain termination retains the fixture after exporting evidence', async () => {
    const events = []
    const result = await SmbObservation.finish({
        exportEvidence: async () => {
            events.push('export')
        },
        cleanup: async () => {
            events.push('cleanup')
        },
        retainFixture: false,
        uncertain: true
    })
    assert.deepEqual(events, ['export'])
    assert.deepEqual(result, { exported: true, retained: true })
})

test('explicit fixture retention still exports evidence and skips cleanup', async () => {
    const events = []
    const result = await SmbObservation.finish({
        exportEvidence: async () => {
            events.push('export')
        },
        cleanup: async () => {
            events.push('cleanup')
        },
        retainFixture: true,
        uncertain: false
    })
    assert.deepEqual(events, ['export'])
    assert.deepEqual(result, { exported: true, retained: true })
})
