/** Collection names that collide with Fylo's built-in API surface. */
const RESERVED_NAMES = new Set([
    'sql',
    'as',
    'then',
    'db',
    'engine',
    'cache',
    'queue',
    'startup',
    'getDoc',
    'putData',
    'findDocs',
    'patchDoc',
    'delDoc',
    'restoreDoc',
    'getLatest',
    'findDeletedDocs',
    'createCollection',
    'dropCollection',
    'rebuildCollection',
    'inspectCollection',
    'exportBulkData',
    'importBulkData',
    'batchPutData',
    'patchDocs',
    'delDocs',
    'executeSQL',
    'joinDocs',
    'ready',
    'close',
    'sql',
    '_sql'
])

/**
 * @param {string} collection
 */
export function validateCollectionName(collection) {
    if (!/^[a-z0-9][a-z0-9\-]*[a-z0-9]$/.test(collection)) {
        throw new Error('Invalid collection name')
    }
    if (RESERVED_NAMES.has(collection)) {
        throw new Error(
            `'${collection}' is a reserved name and cannot be used as a collection name. ` +
                `Choose a different name.`
        )
    }
}
