import { afterAll, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, readFile, rename, rm, symlink, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import {
    closeSecureDescriptor,
    libcCandidates,
    loadSecureOpenSymbols,
    openDirectoryNoFollow,
    unlinkAtRoot
} from '../../src/storage/secure-open.js'
import {
    WINDOWS_NATIVE_CONSTANTS,
    WINDOWS_NATIVE_LAYOUT,
    retryWindowsRenameAccessDenied
} from '../../src/storage/windows-secure-open.js'

const root = await mkdtemp(path.join(os.tmpdir(), 'fylo-secure-open-'))

afterAll(async () => {
    await rm(root, { recursive: true, force: true })
})

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

    test('pins the audited 64-bit Windows SDK layouts and fail-closed flags', () => {
        expect(WINDOWS_NATIVE_LAYOUT).toMatchObject({
            pointerBytes: 8,
            unicodeStringBytes: 16,
            objectAttributesBytes: 48,
            ioStatusBlockBytes: 16,
            overlappedBytes: 32,
            fileRenameHeaderBytes: 20,
            fileRenameStructBytes: 24
        })
        expect(
            WINDOWS_NATIVE_CONSTANTS.FILE_OPEN_REPARSE_POINT &
                WINDOWS_NATIVE_CONSTANTS.FILE_OPEN_REPARSE_POINT
        ).not.toBe(0)
        expect(WINDOWS_NATIVE_CONSTANTS.FILE_RENAME_INFORMATION).toBe(10)
        expect(WINDOWS_NATIVE_CONSTANTS.FILE_DISPOSITION_INFO_EX).toBe(21)
        expect(WINDOWS_NATIVE_CONSTANTS.FILE_ATTRIBUTE_REPARSE_POINT).toBe(0x400)
        expect(WINDOWS_NATIVE_CONSTANTS.FILE_ATTRIBUTE_READONLY).toBe(0x1)
    })

    test('retries transient Windows access denials with bounded backoff', () => {
        const statuses = [
            WINDOWS_NATIVE_CONSTANTS.STATUS_ACCESS_DENIED,
            WINDOWS_NATIVE_CONSTANTS.STATUS_ACCESS_DENIED,
            0
        ]
        const waits = []

        const status = retryWindowsRenameAccessDenied(
            () => /** @type {number} */ (statuses.shift()),
            (delayMs) => waits.push(delayMs)
        )

        expect(status).toBe(0)
        expect(waits).toEqual([10, 20])
        expect(statuses).toEqual([])
    })

    test('does not retry permanent Windows rename failures', () => {
        let attempts = 0
        const permanentFailure = WINDOWS_NATIVE_CONSTANTS.STATUS_OBJECT_PATH_NOT_FOUND

        const status = retryWindowsRenameAccessDenied(
            () => {
                attempts += 1
                return permanentFailure
            },
            () => {
                throw new Error('permanent failures must not wait')
            }
        )

        expect(status).toBe(permanentFailure)
        expect(attempts).toBe(1)
    })

    test('returns a persistent access denial after the bounded retry budget', () => {
        let attempts = 0
        const waits = []

        const status = retryWindowsRenameAccessDenied(
            () => {
                attempts += 1
                return WINDOWS_NATIVE_CONSTANTS.STATUS_ACCESS_DENIED
            },
            (delayMs) => waits.push(delayMs)
        )

        expect(status).toBe(WINDOWS_NATIVE_CONSTANTS.STATUS_ACCESS_DENIED)
        expect(attempts).toBe(8)
        expect(waits).toEqual([10, 20, 40, 80, 160, 320, 640])
    })

    test('a directory-to-symlink swap cannot redirect a rooted mutation', async () => {
        const collection = path.join(root, 'collection')
        const original = path.join(collection, 'docs')
        const displaced = path.join(collection, 'docs-displaced')
        const outside = path.join(root, 'outside')
        await mkdir(original, { recursive: true })
        await mkdir(outside, { recursive: true })
        await writeFile(path.join(original, 'sentinel.txt'), 'inside')
        await writeFile(path.join(outside, 'sentinel.txt'), 'outside')

        const rootFd = openDirectoryNoFollow(collection)
        try {
            // Models the attacker winning the exact gap between root
            // validation and mutation.
            await rename(original, displaced)
            await symlink(outside, original, process.platform === 'win32' ? 'junction' : 'dir')
            expect(() => unlinkAtRoot(rootFd, 'docs/sentinel.txt')).toThrow()
        } finally {
            closeSecureDescriptor(rootFd)
        }
        expect(await readFile(path.join(outside, 'sentinel.txt'), 'utf8')).toBe('outside')
        expect(await readFile(path.join(displaced, 'sentinel.txt'), 'utf8')).toBe('inside')
    })
})
