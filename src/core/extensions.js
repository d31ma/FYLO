import { copySafeRecord, safeRecord } from '../query/safe-record.js'

/**
 * @param {Record<string, any>} target
 * @param {Record<string, any>} source
 * @returns {Record<string, any>}
 */
export function appendGroup(target, source) {
    const result = copySafeRecord(target)
    for (const [sourceId, sourceGroup] of Object.entries(source)) {
        if (!Object.hasOwn(result, sourceId)) {
            result[sourceId] =
                sourceGroup && typeof sourceGroup === 'object' && !Array.isArray(sourceGroup)
                    ? copySafeRecord(sourceGroup)
                    : sourceGroup
            continue
        }
        result[sourceId] =
            result[sourceId] &&
            typeof result[sourceId] === 'object' &&
            !Array.isArray(result[sourceId])
                ? copySafeRecord(result[sourceId])
                : safeRecord()
        for (const [groupId, groupDoc] of Object.entries(sourceGroup)) {
            result[sourceId][groupId] = groupDoc
        }
    }
    return result
}

Object.assign(Object, { appendGroup })
