import { formatTable } from './format.js'

/**
 * @typedef {import('./format.js').FormatTableOptions} FormatTableOptions
 */

const DEFAULT_PAGER = 'less -FRX'
const DISABLED_PAGER_VALUES = new Set(['0', 'false', 'off', 'none', 'disabled'])

/** @typedef {'auto' | 'never'} PagerMode */

/**
 * @typedef {object} CliOutputOptions
 * @property {PagerMode=} pagerMode
 * @property {string=} pagerCommand
 * @property {Pick<typeof process, 'env' | 'stdin' | 'stdout'>=} processLike
 */

/**
 * @param {Record<string, any>} docs
 * @param {FormatTableOptions=} tableOptions
 */
export async function renderTableOutput(docs, tableOptions = {}) {
    return await formatTable(docs, tableOptions)
}

/**
 * @param {string} input
 * @returns {string[]}
 */
export function splitCommandLine(input) {
    const parts = []
    let current = ''
    let quote = null
    let escaped = false
    for (const char of input) {
        if (escaped) {
            current += char
            escaped = false
            continue
        }
        if (char === '\\') {
            escaped = true
            continue
        }
        if (quote) {
            if (char === quote) quote = null
            else current += char
            continue
        }
        if (char === '"' || char === "'") {
            quote = char
            continue
        }
        if (/\s/.test(char)) {
            if (current.length > 0) {
                parts.push(current)
                current = ''
            }
            continue
        }
        current += char
    }
    if (current.length > 0) parts.push(current)
    return parts
}

/**
 * @param {Pick<typeof process, 'env' | 'stdin' | 'stdout'>=} processLike
 * @returns {string | undefined}
 */
export function resolvePagerCommand(processLike = process) {
    if (processLike.env.NO_PAGER) return undefined
    const configured = processLike.env.FYLO_PAGER?.trim()
    if (configured && DISABLED_PAGER_VALUES.has(configured.toLowerCase())) return undefined
    if (configured) return configured
    const pager = processLike.env.PAGER?.trim()
    if (pager && DISABLED_PAGER_VALUES.has(pager.toLowerCase())) return undefined
    if (pager) return pager
    return DEFAULT_PAGER
}

/**
 * @param {string} text
 * @param {PagerMode=} mode
 * @param {Pick<typeof process, 'env' | 'stdin' | 'stdout'>=} processLike
 * @returns {boolean}
 */
export function shouldUsePager(text, mode = 'auto', processLike = process) {
    if (mode === 'never') return false
    if (!text.trim()) return false
    if (!processLike.stdin?.isTTY || !processLike.stdout?.isTTY) return false
    if (!resolvePagerCommand(processLike)) return false
    const rowsFromEnv = Number(processLike.env.LINES)
    const terminalRows =
        processLike.stdout.rows ??
        (Number.isFinite(rowsFromEnv) && rowsFromEnv > 0 ? Math.floor(rowsFromEnv) : 24)
    return text.split('\n').length > Math.max(terminalRows - 1, 1)
}

/**
 * @param {string} text
 * @param {CliOutputOptions=} options
 * @returns {Promise<void>}
 */
export async function writeCliText(text, options = {}) {
    const processLike = options.processLike ?? process
    if (!shouldUsePager(text, options.pagerMode ?? 'auto', processLike)) {
        console.log(text)
        return
    }
    const pagerCommand = options.pagerCommand ?? resolvePagerCommand(processLike)
    const argv = pagerCommand ? splitCommandLine(pagerCommand) : []
    if (argv.length === 0) {
        console.log(text)
        return
    }
    try {
        const proc = Bun.spawn(argv, {
            stdin: new Blob([text]),
            stdout: 'inherit',
            stderr: 'inherit'
        })
        const exitCode = await proc.exited
        if (exitCode !== 0) console.log(text)
    } catch {
        console.log(text)
    }
}
