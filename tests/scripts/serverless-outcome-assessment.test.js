import assert from 'node:assert/strict'
import { test } from 'node:test'
import { ServerlessOutcomeAssessment } from '../../scripts/serverless-outcome-assessment.mjs'

const id = '4VRNF52JR00'
const other = '4VRNF52JR10'

test('a failed read preserves its response, error and acknowledged IDs as unknown', () => {
    const response = { outcome: { ok: false, error: { code: 'EIO', message: 'read failed' } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id, other])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.known, false)
    assert.equal(assessment.response, response)
    assert.equal(assessment.response.outcome.error, response.outcome.error)
    assert.deepEqual(assessment.acknowledged, [id, other])
    assert.deepEqual(assessment.missing, [])
})

test('a missing response leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess(undefined, [id])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('a missing outcome leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ body: 'invalid response' }, [id])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('an incomplete runtime response preserves its error as unknown', () => {
    const response = { outcome: { noCompletedResponse: 'deadline elapsed' } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('a successful response without a result leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true } }, [id])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('a null result leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: null } }, [
        id
    ])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('a string result containing the ID leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: id } }, [
        id
    ])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('a numeric result leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: 1 } }, [
        id
    ])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('a boolean result leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: true } }, [
        id
    ])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('an array result leaves survivors unknown', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: [id] } }, [
        id
    ])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('an ID appearing only in a document value is missing', () => {
    const assessment = ServerlessOutcomeAssessment.assess(
        { outcome: { ok: true, result: { [other]: { name: id } } } },
        [id]
    )
    assert.equal(assessment.status, 'missing')
    assert.equal(assessment.known, true)
    assert.deepEqual(assessment.missing, [id])
})

test('an inherited document key does not establish survival', () => {
    const result = Object.create({ [id]: { name: id } })
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result } }, [id])
    assert.equal(assessment.status, 'unknown')
    assert.deepEqual(assessment.missing, [])
})

test('an exact own document key establishes survival', () => {
    const result = Object.assign(Object.create(null), { [id]: { name: id } })
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result } }, [id])
    assert.equal(assessment.status, 'survived')
    assert.equal(assessment.known, true)
    assert.deepEqual(assessment.missing, [])
})

test('a successful empty listing reports the missing acknowledged IDs', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: {} } }, [
        id,
        other
    ])
    assert.equal(assessment.status, 'missing')
    assert.deepEqual(assessment.missing, [id, other])
})

test('only acknowledged IDs are assessed for survival', () => {
    const assessment = ServerlessOutcomeAssessment.assess(
        { outcome: { ok: true, result: { [id]: {} } } },
        [id]
    )
    assert.equal(assessment.status, 'survived')
    assert.deepEqual(assessment.acknowledged, [id])
    assert.deepEqual(assessment.missing, [])
})

test('zero acknowledgments are inconclusive even with a successful listing', () => {
    const assessment = ServerlessOutcomeAssessment.assess(
        { outcome: { ok: true, result: { [id]: {} } } },
        []
    )
    assert.equal(assessment.status, 'inconclusive')
    assert.equal(assessment.known, true)
    assert.deepEqual(assessment.acknowledged, [])
    assert.deepEqual(assessment.missing, [])
})

test('a rejected read preserves the actual transport error and acknowledgments', async () => {
    const error = new Error('curl timed out')
    const assessment = await ServerlessOutcomeAssessment.read(() => Promise.reject(error), [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.error, error)
    assert.equal(assessment.response, undefined)
    assert.deepEqual(assessment.acknowledged, [id])
    assert.deepEqual(assessment.missing, [])
})

test('a resolved read preserves its response and assesses exact keys', async () => {
    const response = { outcome: { ok: true, result: { [id]: {} } } }
    const assessment = await ServerlessOutcomeAssessment.read(() => Promise.resolve(response), [id])
    assert.equal(assessment.status, 'survived')
    assert.equal(assessment.response, response)
    assert.equal(assessment.error, null)
})

test('a synchronous read failure remains unknown', async () => {
    const error = new Error('could not start read')
    const assessment = await ServerlessOutcomeAssessment.read(() => {
        throw error
    }, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.error, error)
    assert.deepEqual(assessment.missing, [])
})

test('an exotic object result leaves survivors unknown', () => {
    const response = { outcome: { ok: true, result: new Date(0) } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('a read rejecting null preserves the rejection separately from its value', async () => {
    const assessment = await ServerlessOutcomeAssessment.read(() => Promise.reject(null), [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.rejected, true)
    assert.equal(assessment.error, null)
    assert.deepEqual(assessment.acknowledged, [id])
    assert.deepEqual(assessment.missing, [])
})

test('a read rejecting undefined preserves the rejection separately from its value', async () => {
    const assessment = await ServerlessOutcomeAssessment.read(() => Promise.reject(undefined), [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.rejected, true)
    assert.equal(assessment.error, undefined)
    assert.deepEqual(assessment.acknowledged, [id])
    assert.deepEqual(assessment.missing, [])
})

test('an acknowledged ID mapped to null leaves survivors unknown', () => {
    const response = { outcome: { ok: true, result: { [id]: null } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.known, false)
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.acknowledged, [id])
    assert.deepEqual(assessment.missing, [])
})

test('an acknowledged ID mapped to a string leaves survivors unknown', () => {
    const response = { outcome: { ok: true, result: { [id]: id } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('an acknowledged ID mapped to a number leaves survivors unknown', () => {
    const response = { outcome: { ok: true, result: { [id]: 1 } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('an acknowledged ID mapped to an array leaves survivors unknown', () => {
    const response = { outcome: { ok: true, result: { [id]: [{ name: id }] } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('a malformed unacknowledged document invalidates the listing', () => {
    const response = { outcome: { ok: true, result: { [id]: {}, [other]: null } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('an acknowledged ID mapped to an exotic object leaves survivors unknown', () => {
    const response = { outcome: { ok: true, result: { [id]: new Date(0) } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'unknown')
    assert.equal(assessment.response, response)
    assert.deepEqual(assessment.missing, [])
})

test('a null-prototype document record establishes survival', () => {
    const response = { outcome: { ok: true, result: { [id]: Object.create(null) } } }
    const assessment = ServerlessOutcomeAssessment.assess(response, [id])
    assert.equal(assessment.status, 'survived')
    assert.equal(assessment.known, true)
    assert.deepEqual(assessment.missing, [])
})

test('an empty listing with zero acknowledgments remains valid and inconclusive', () => {
    const assessment = ServerlessOutcomeAssessment.assess({ outcome: { ok: true, result: {} } }, [])
    assert.equal(assessment.status, 'inconclusive')
    assert.equal(assessment.known, true)
    assert.deepEqual(assessment.missing, [])
})
