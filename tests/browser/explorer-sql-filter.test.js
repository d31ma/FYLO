import { describe, expect, test } from 'bun:test'
import { createBrowserClient } from '../../src/browser/client.js'
import WebsiteExplorer, {
    EXPLORER_LIMITS,
    browserLabel,
    engineAssetUrl,
    resizeValueForKey
} from '../../website/client/components/explorer/app/tac.js'

describe('SQL WHERE execution', () => {
    test('requires every AND condition to match', async () => {
        const db = createBrowserClient({ worker: false })
        await db.users.create()
        const admin = await db.users.put({ role: 'admin', age: 30 })
        await db.users.put({ role: 'member', age: 40 })

        expect(await db._sql("SELECT _id FROM users WHERE role = 'admin' AND age >= 30")).toEqual([
            admin
        ])

        await db.close()
    })
})

describe('hosted Explorer', () => {
    test('resizes separators with arrows and bounded Home/End keyboard controls', () => {
        expect(resizeValueForKey(200, 'ArrowLeft')).toBe(190)
        expect(resizeValueForKey(200, 'ArrowRight')).toBe(210)
        expect(resizeValueForKey(200, 'ArrowUp', true)).toBe(250)
        expect(resizeValueForKey(200, 'ArrowDown', true)).toBe(150)
        expect(resizeValueForKey(200, 'Home')).toBe(120)
        expect(resizeValueForKey(200, 'End')).toBe(1200)
        expect(resizeValueForKey(120, 'ArrowLeft')).toBe(120)
        expect(resizeValueForKey(1200, 'ArrowRight')).toBe(1200)
        expect(resizeValueForKey(200, 'Enter')).toBeNull()
    })

    test('names the current unsupported browser for compatibility guidance', () => {
        expect(browserLabel('Mozilla/5.0 Firefox/140.0')).toBe('Firefox')
        expect(browserLabel('Mozilla/5.0 Version/18.5 Safari/605.1.15')).toBe('Safari')
        expect(browserLabel('unknown')).toBe('This browser')
    })

    test('constructs only the allowlisted same-origin engine asset URL', () => {
        expect(engineAssetUrl('26.28.07', 'https://fylo.example')).toBe(
            'https://fylo.example/shared/assets/fylo-web.mjs?v=26.28.07'
        )
        expect(() => engineAssetUrl('../evil', 'https://fylo.example')).toThrow(
            'Invalid FYLO Explorer build token'
        )
        expect(() => engineAssetUrl('ok', 'data:text/html,evil')).toThrow(
            'Invalid FYLO Explorer asset origin'
        )
    })

    test('rejects oversized raw previews before reading bytes', async () => {
        const explorer = new WebsiteExplorer()
        explorer.active = 'assets'
        explorer.kinds = { assets: 'file' }
        explorer.selectedId = 'doc-1'
        let read = false
        explorer._fs = {
            async list() {
                return ['doc-1.bin']
            },
            async size() {
                return EXPLORER_LIMITS.rawPreviewBytes + 1
            },
            async readBytes() {
                read = true
                return new Uint8Array()
            }
        }

        await explorer.loadRaw('doc-1')

        expect(read).toBe(false)
        expect(explorer.error).toContain('Raw preview is limited to 32 MiB')
    })

    test('rejects oversized imports before reading the File', async () => {
        const explorer = new WebsiteExplorer()
        explorer.writable = true
        let read = false
        await explorer.doImport({
            collection: 'users',
            file: {
                size: EXPLORER_LIMITS.importBytes + 1,
                async text() {
                    read = true
                    return '[]'
                }
            }
        })

        expect(read).toBe(false)
        expect(explorer.error).toContain('Import is limited to 16 MiB')
    })

    test('rejects oversized exports before reading document contents', async () => {
        const explorer = new WebsiteExplorer()
        explorer._rebuilt = new Set(['users'])
        let read = false
        explorer._fs = {
            async size() {
                return EXPLORER_LIMITS.exportBytes + 1
            }
        }
        explorer._db = {
            collection() {
                return {
                    find() {
                        return {
                            async *collect() {
                                yield ['4VNQ8ZROGVW']
                            }
                        }
                    },
                    get() {
                        read = true
                        return {
                            async once() {
                                return {}
                            }
                        }
                    }
                }
            }
        }

        await explorer.doExport('users')

        expect(read).toBe(false)
        expect(explorer.error).toContain('Export is limited to 64 MiB')
    })

    test('bounds imported record count and preserves reserved keys as own data', () => {
        const explorer = new WebsiteExplorer()
        const record = explorer.parseImport(
            '[{"__proto__":"proto","constructor":"ctor","prototype":"prototype"}]'
        )[0]
        expect(Object.getPrototypeOf(record)).toBeNull()
        expect(record.__proto__).toBe('proto')
        expect(record.constructor).toBe('ctor')
        expect(() =>
            explorer.parseImport(
                Array.from({ length: EXPLORER_LIMITS.importRecords + 1 }, () => '{}').join('\n')
            )
        ).toThrow('Import is limited to 10,000 records')
    })

    test('filters a collection with a SQL WHERE expression', async () => {
        const explorer = new WebsiteExplorer()
        const statements = []
        const rebuilt = []
        explorer.active = 'users'
        explorer.filter = "role = 'admin' AND age >= 30"
        explorer._rebuilt = new Set()
        explorer._db = {
            collection(name) {
                return {
                    async rebuild() {
                        rebuilt.push(name)
                    }
                }
            },
            async _sql(statement) {
                statements.push(statement)
                return ['4VNQ8ZROGVW', '4VNQ8ZRHXKA']
            }
        }

        await explorer.applyFilter()

        expect(rebuilt).toEqual(['users'])
        expect(statements).toEqual(["SELECT _id FROM users WHERE role = 'admin' AND age >= 30"])
        expect(explorer.filtered).toEqual(['4VNQ8ZRHXKA', '4VNQ8ZROGVW'])
    })

    test('accepts an optional WHERE prefix', async () => {
        const explorer = new WebsiteExplorer()
        const statements = []
        explorer.active = 'users'
        explorer.filter = 'WHERE active = true'
        explorer._rebuilt = new Set(['users'])
        explorer._db = {
            collection() {
                throw new Error('an already rebuilt collection must not rebuild')
            },
            async _sql(statement) {
                statements.push(statement)
                return []
            }
        }

        await explorer.applyFilter()

        expect(statements).toEqual(['SELECT _id FROM users WHERE active = true'])
        expect(explorer.filtered).toEqual([])
    })

    test('suggests active collection fields and SQL WHERE keywords', () => {
        const explorer = new WebsiteExplorer()
        explorer.filterColumns = ['age', 'role']

        explorer.filterInput({ target: { value: 'ro', selectionStart: 2 } })
        expect(explorer.filterSuggest).toEqual([{ text: 'role', kind: 'col' }])

        explorer.filterInput({ target: { value: 'an', selectionStart: 2 } })
        expect(explorer.filterSuggest).toEqual([{ text: 'AND', kind: 'kw' }])
    })

    test('reports an empty SQL result without rendering an empty object', async () => {
        const explorer = new WebsiteExplorer()
        explorer.view = 'sql'
        explorer.sqlText = "SELECT * FROM users WHERE role = 'missing'"
        explorer.collections = []
        explorer._db = {
            async _sql() {
                return {}
            }
        }

        await explorer.runSql()

        expect(explorer.sqlEmpty).toBe(true)
        expect(explorer.sqlResult).toBe('')
        expect(explorer.sqlRows).toEqual([])
        expect(explorer.hasDoc()).toBe(true)
    })
})
