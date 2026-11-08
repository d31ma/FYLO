// Names shared by the page and the `.vdbx` Worker (ADR 0016 section 10).

/** The OPFS directory that holds Explorer's working copies. */
export const WORKING_COPY_DIRECTORY = 'fylo-vdbx'

/**
 * Working copy names: one plain file name, nothing hidden, `.vdbx` at the end.
 *
 * @param {unknown} name
 * @returns {name is string}
 */
export function isWorkingCopyName(name) {
    return (
        typeof name === 'string' &&
        name.length <= 200 &&
        /^[A-Za-z0-9][A-Za-z0-9 ._()-]*\.vdbx$/.test(name) &&
        !name.includes('..')
    )
}
