import { isDeepStrictEqual } from 'node:util'
import { ServerlessOutcomeAssessment } from './serverless-outcome-assessment.mjs'

export class SmbObservationEvidence {
    static sourceUrl = import.meta.url

    static decode(transport, request) {
        return SmbObservationEvidence.#decode(transport, request)
    }

    static acknowledged(request, transport) {
        return SmbObservationEvidence.#acknowledged(request, transport)
    }

    static assess(writes, read) {
        return SmbObservationEvidence.#assess(writes, read)
    }

    static #decode(transport, request) {
        const raw = transport?.stdout ?? ''
        const { headers, httpStatus, body } = SmbObservationEvidence.#http(raw)
        let response
        let parseError = null
        try {
            response = JSON.parse(body)
        } catch (error) {
            parseError = { name: error.name, message: error.message }
        }
        const complete = SmbObservationEvidence.#complete(transport, httpStatus, parseError)
        return { request, transport, httpStatus, headers, body, response, parseError, complete }
    }

    static #http(raw) {
        let body = raw
        let headers = null
        let httpStatus = null
        do {
            const separator = /\r?\n\r?\n/.exec(body)
            if (!separator) break
            const status = /^HTTP\/\S+\s+(\d{3})(?:[^\r\n]*)(?:\r?\n[\s\S]*)?$/.exec(
                body.slice(0, separator.index)
            )
            if (!status) break
            const end = separator.index + separator[0].length
            headers = body.slice(0, end)
            httpStatus = Number(status[1])
            body = body.slice(end)
        } while (httpStatus >= 100 && httpStatus < 200)
        return { headers, httpStatus, body }
    }

    static #complete(transport, status, parseError) {
        return Boolean(
            transport &&
            transport.exitCode === 0 &&
            !transport.signal &&
            !transport.timedOut &&
            !transport.evidenceIncomplete &&
            !transport.inputError &&
            !transport.error &&
            status >= 200 &&
            status < 300 &&
            parseError === null
        )
    }

    static #acknowledged(request, transport) {
        const decoded = SmbObservationEvidence.decode(transport, request)
        const outcome = decoded.response?.outcome
        if (
            !decoded.complete ||
            request?.op !== 'putData' ||
            !/^[A-Z0-9]{11}$/.test(request.id ?? '') ||
            outcome?.ok !== true
        )
            return false
        if (outcome.op !== undefined && outcome.op !== request.op) return false
        if (outcome.id !== undefined && outcome.id !== request.id) return false
        if (outcome.result?.id !== undefined && outcome.result.id !== request.id) return false
        if (typeof outcome.result === 'string' && outcome.result !== request.id) return false
        return outcome.result === request.id
    }

    static #assess(writes, read) {
        const acknowledged = writes.filter(({ request, transport }) =>
            SmbObservationEvidence.acknowledged(request, transport)
        )
        const ids = acknowledged.map(({ request }) => request.id)
        const decoded = SmbObservationEvidence.decode(read.transport, read.request)
        const scope =
            typeof read.request?.root === 'string' &&
            read.request.root.length > 0 &&
            typeof read.request?.collection === 'string' &&
            read.request.collection.length > 0 &&
            read.request?.op === 'findDocs' &&
            isDeepStrictEqual(read.request.query, {}) &&
            writes.every(
                ({ request }) =>
                    request.collection === read.request.collection &&
                    request.root === read.request.root
            ) &&
            (decoded.response?.outcome?.op === undefined ||
                decoded.response.outcome.op === 'findDocs')
        const response = decoded.complete && scope ? decoded.response : undefined
        const assessment = ServerlessOutcomeAssessment.assess(response, ids)
        const present = assessment.known ? ids.filter((id) => !assessment.missing.includes(id)) : []
        const mismatchedPayloads = SmbObservationEvidence.#payloadMismatches(
            assessment,
            acknowledged,
            response
        )
        return {
            ...assessment,
            response: decoded.response,
            transport: read.transport,
            decoded,
            scope,
            present,
            mismatchedPayloads,
            payloadStatus: SmbObservationEvidence.#payloadStatus(assessment, mismatchedPayloads),
            interpretation: 'Presence or absence in this read; no attribution of storage loss'
        }
    }

    static #payloadMismatches(assessment, acknowledged, response) {
        if (!assessment.known) return []
        return acknowledged
            .filter(
                ({ request }) =>
                    Object.hasOwn(response.outcome.result, request.id) &&
                    !isDeepStrictEqual(response.outcome.result[request.id], request.data)
            )
            .map(({ request }) => request.id)
    }

    static #payloadStatus(assessment, mismatched) {
        switch (true) {
            case !assessment.known:
                return 'unknown'
            case assessment.acknowledged.length === 0:
                return 'inconclusive'
            case mismatched.length !== 0:
                return 'mismatch'
            case assessment.missing.length !== 0:
                return 'incomplete'
            default:
                return 'matched'
        }
    }
}
