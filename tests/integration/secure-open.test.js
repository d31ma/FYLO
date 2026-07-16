import { describe, expect, test } from 'bun:test'
import { libcCandidates, loadSecureOpenSymbols } from '../../src/storage/secure-open.js'

describe('secure descriptor traversal availability', () => {
    test('covers glibc and Alpine musl library names', () => {
        expect(libcCandidates('linux', 'x64')).toEqual([
            'libc.so.6',
            'libc.musl-x86_64.so.1',
            '/lib/ld-musl-x86_64.so.1'
        ])
    })

    test('tries compatible libc candidates and fails closed without breaking module import', () => {
        const attempts = []
        const symbols = loadSecureOpenSymbols(
            (candidate) => {
                attempts.push(candidate)
                throw new Error(`missing ${candidate}`)
            },
            ['libc.so.6', 'libc.musl-x86_64.so.1']
        )

        expect(symbols).toBeNull()
        expect(attempts).toEqual(['libc.so.6', 'libc.musl-x86_64.so.1'])
    })

    test('uses the first libc candidate that exposes openat and errno', () => {
        const expected = { openat() {}, __errno_location() {} }
        const symbols = loadSecureOpenSymbols(
            (candidate) => {
                if (candidate === 'glibc') throw new Error('not installed')
                return { symbols: expected }
            },
            ['glibc', 'musl']
        )

        expect(symbols).toBe(expected)
    })
})
