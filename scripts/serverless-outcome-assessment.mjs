export class ServerlessOutcomeAssessment {
    static assess(response, acknowledged) {
        return ServerlessOutcomeAssessment.#assessment(response, acknowledged, null, false)
    }

    static async read(operation, acknowledged) {
        return ServerlessOutcomeAssessment.#read(operation, acknowledged)
    }

    static #assessment(response, acknowledged, error, rejected) {
        const listing = rejected ? null : ServerlessOutcomeAssessment.#listing(response)
        const known = listing !== null
        const missing = ServerlessOutcomeAssessment.#missing(listing, acknowledged)
        const status = ServerlessOutcomeAssessment.#status(known, acknowledged, missing)
        return {
            status,
            known,
            acknowledged: [...acknowledged],
            missing,
            response,
            error,
            rejected
        }
    }

    static #listing(response) {
        const result = response?.outcome?.result
        if (response?.outcome?.ok !== true || !ServerlessOutcomeAssessment.#isListing(result)) {
            return null
        }
        return result
    }

    static #isListing(value) {
        return (
            ServerlessOutcomeAssessment.#isRecord(value) &&
            Object.values(value).every((document) =>
                ServerlessOutcomeAssessment.#isRecord(document)
            )
        )
    }

    static #isRecord(value) {
        if (value === null || typeof value !== 'object' || Array.isArray(value)) return false
        const prototype = Object.getPrototypeOf(value)
        return prototype === Object.prototype || prototype === null
    }

    static #missing(listing, acknowledged) {
        if (listing === null) return []
        return acknowledged.filter((id) => !Object.hasOwn(listing, id))
    }

    static #status(known, acknowledged, missing) {
        switch (true) {
            case !known:
                return 'unknown'
            case acknowledged.length === 0:
                return 'inconclusive'
            case missing.length !== 0:
                return 'missing'
            default:
                return 'survived'
        }
    }

    static async #read(operation, acknowledged) {
        let response
        try {
            response = await operation()
        } catch (error) {
            return ServerlessOutcomeAssessment.#assessment(undefined, acknowledged, error, true)
        }
        return ServerlessOutcomeAssessment.assess(response, acknowledged)
    }
}
