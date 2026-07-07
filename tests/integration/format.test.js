import { describe, expect, test } from 'bun:test'
import { formatTable } from '../../src/cli/format.js'

describe('formatTable', async () => {
    test('returns a safe empty-state message for empty objects', async () => {
        expect(await formatTable({})).toBe('(no rows)')
    })

    test('formats flat rows without mutating the global console', async () => {
        const output = await formatTable({
            alpha: { name: 'Ada', active: true }
        })

        expect(output).toContain('│ _key')
        expect(output).toContain('│ alpha ')
        expect(output).toContain('│ name ')
        expect(output).toContain('│ active ')
        expect(typeof console.format).toBe('undefined')
    })

    test('flattens nested objects into stable dotted columns', async () => {
        const output = await formatTable({
            alpha: { profile: { city: 'Toronto', zip: 12345 }, role: 'admin' }
        })

        expect(output).toContain('profile.city')
        expect(output).toContain('profile.zip')
        expect(output).toContain('role')
        expect(output).toContain('Toronto')
        expect(output).toContain('12345')
    })

    test('truncates long values and keeps unicode content printable', async () => {
        const output = await formatTable(
            {
                alpha: {
                    emoji: '😀😀😀😀😀😀😀😀😀😀',
                    note: 'This value should be truncated because it is very long'
                }
            },
            { maxColumnWidth: 12 }
        )

        expect(output).toContain('😀😀😀...')
        expect(output).toContain('This valu...')
    })

    test('wraps long values across multiple lines when enabled', async () => {
        const output = await formatTable(
            {
                alpha: {
                    note: 'This value should wrap across multiple lines cleanly'
                }
            },
            { maxColumnWidth: 12, wrap: true }
        )

        expect(output).toContain('This value')
        expect(output).toContain('should wrap')
        expect(output).toContain('multiple')
        expect(output).toContain('cleanly')
    })

    test('fits columns to the provided terminal width', async () => {
        const output = await formatTable(
            {
                alpha: {
                    first: 'value-one',
                    second: 'value-two',
                    third: 'value-three'
                }
            },
            { terminalWidth: 32, wrap: true }
        )

        for (const line of output.split('\n')) {
            expect(line.length).toBeLessThanOrEqual(32)
        }
    })

    test('supports right alignment for cell values', async () => {
        const output = await formatTable(
            {
                alpha: { count: 12 }
            },
            { cellAlign: 'right' }
        )

        expect(output).toContain('│ alpha │    12 │')
    })

    test('repeats headers across pages when page size is set', async () => {
        const output = await formatTable(
            {
                alpha: { name: 'Ada' },
                beta: { name: 'Grace' },
                gamma: { name: 'Linus' }
            },
            { pageSize: 2 }
        )

        expect(output.split('│ _key ').length - 1).toBe(2)
    })
})
