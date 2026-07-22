import { dlopen, FFIType, ptr, read } from 'bun:ffi'
import { closeSync, constants, fsyncSync } from 'node:fs'
import path from 'node:path'

// Native values are intentionally kept here instead of copied into callers.
// The layouts below are the x64/arm64 Windows SDK layouts; 32-bit Windows is
// rejected because FYLO does not publish a 32-bit executable.
export const WINDOWS_NATIVE_LAYOUT = Object.freeze({
    pointerBytes: 8,
    unicodeStringBytes: 16,
    objectAttributesBytes: 48,
    ioStatusBlockBytes: 16,
    overlappedBytes: 32,
    fileRenameHeaderBytes: 20
})

export const WINDOWS_NATIVE_CONSTANTS = Object.freeze({
    INVALID_HANDLE_VALUE: -1n,
    LOCKFILE_FAIL_IMMEDIATELY: 0x1,
    LOCKFILE_EXCLUSIVE_LOCK: 0x2,
    ERROR_LOCK_VIOLATION: 33,
    OBJ_CASE_INSENSITIVE: 0x40,
    FILE_SHARE_READ: 0x1,
    FILE_SHARE_WRITE: 0x2,
    FILE_SHARE_DELETE: 0x4,
    FILE_OPEN: 0x1,
    FILE_CREATE: 0x2,
    FILE_OPEN_IF: 0x3,
    FILE_OVERWRITE: 0x4,
    FILE_OVERWRITE_IF: 0x5,
    FILE_DIRECTORY_FILE: 0x1,
    FILE_SYNCHRONOUS_IO_NONALERT: 0x20,
    FILE_NON_DIRECTORY_FILE: 0x40,
    FILE_OPEN_REPARSE_POINT: 0x200000,
    FILE_READ_DATA: 0x1,
    FILE_WRITE_DATA: 0x2,
    FILE_APPEND_DATA: 0x4,
    FILE_READ_ATTRIBUTES: 0x80,
    FILE_WRITE_ATTRIBUTES: 0x100,
    DELETE: 0x10000,
    SYNCHRONIZE: 0x100000,
    FILE_ATTRIBUTE_NORMAL: 0x80,
    FILE_ATTRIBUTE_DIRECTORY: 0x10,
    FILE_FLAG_BACKUP_SEMANTICS: 0x02000000,
    FILE_FLAG_OPEN_REPARSE_POINT: 0x00200000,
    OPEN_EXISTING: 3,
    // FILE_INFO_BY_HANDLE_CLASS (Win32), not the similarly named NT enum.
    FILE_RENAME_INFO_EX: 22,
    FILE_DISPOSITION_INFO_EX: 21,
    FILE_RENAME_FLAG_REPLACE_IF_EXISTS: 0x1,
    FILE_RENAME_FLAG_POSIX_SEMANTICS: 0x2,
    FILE_DISPOSITION_FLAG_DELETE: 0x1,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: 0x2,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: 0x10,
    STATUS_OBJECT_NAME_NOT_FOUND: -1073741772,
    STATUS_OBJECT_PATH_NOT_FOUND: -1073741766,
    STATUS_NO_SUCH_FILE: -1073741809,
    O_BINARY: 0x8000
})

/** @param {number | bigint} value */
function isInvalidHandle(value) {
    const handle = asBigInt(value)
    return (
        handle === 0n || BigInt.asIntN(64, handle) === WINDOWS_NATIVE_CONSTANTS.INVALID_HANDLE_VALUE
    )
}

/** @type {Record<string, Function> | undefined} */
let native

export function windowsCrtCandidates() {
    return ['msvcrt.dll', 'ucrtbase.dll']
}

function assertSupportedRuntime() {
    if (process.platform !== 'win32')
        throw new Error('Windows native filesystem APIs are unavailable')
    if (!['x64', 'arm64'].includes(process.arch)) {
        throw new Error(
            `Secure Windows filesystem operations require a 64-bit process (got ${process.arch})`
        )
    }
}

function symbols() {
    assertSupportedRuntime()
    if (native) return native
    const kernel32 = dlopen('kernel32.dll', {
        CreateFileW: {
            args: [
                FFIType.ptr,
                FFIType.u32,
                FFIType.u32,
                FFIType.ptr,
                FFIType.u32,
                FFIType.u32,
                FFIType.ptr
            ],
            returns: FFIType.ptr
        },
        CloseHandle: { args: [FFIType.ptr], returns: FFIType.bool },
        LockFileEx: {
            args: [FFIType.ptr, FFIType.u32, FFIType.u32, FFIType.u32, FFIType.u32, FFIType.ptr],
            returns: FFIType.bool
        },
        UnlockFileEx: {
            args: [FFIType.ptr, FFIType.u32, FFIType.u32, FFIType.u32, FFIType.ptr],
            returns: FFIType.bool
        },
        GetLastError: { args: [], returns: FFIType.u32 },
        GetFinalPathNameByHandleW: {
            args: [FFIType.ptr, FFIType.ptr, FFIType.u32, FFIType.u32],
            returns: FFIType.u32
        },
        SetFileInformationByHandle: {
            args: [FFIType.ptr, FFIType.i32, FFIType.ptr, FFIType.u32],
            returns: FFIType.bool
        }
    }).symbols
    const ntdll = dlopen('ntdll.dll', {
        NtCreateFile: {
            args: [
                FFIType.ptr,
                FFIType.u32,
                FFIType.ptr,
                FFIType.ptr,
                FFIType.ptr,
                FFIType.u32,
                FFIType.u32,
                FFIType.u32,
                FFIType.u32,
                FFIType.ptr,
                FFIType.u32
            ],
            returns: FFIType.i32
        },
        RtlNtStatusToDosError: { args: [FFIType.i32], returns: FFIType.u32 }
    }).symbols
    let crt
    // Bun's Windows node:fs compatibility layer currently shares the legacy
    // MSVCRT descriptor table. A descriptor created in UCRTBASE belongs to a
    // different table and node:fs rejects it with EBADF even though its HANDLE
    // is valid. Keep UCRTBASE as a fallback for future Bun distributions.
    for (const candidate of windowsCrtCandidates()) {
        try {
            crt = dlopen(candidate, {
                _get_osfhandle: { args: [FFIType.i32], returns: FFIType.i64 },
                _open_osfhandle: { args: [FFIType.i64, FFIType.i32], returns: FFIType.i32 }
            }).symbols
            break
        } catch {
            // Windows installations supported by Bun have one of these CRTs.
        }
    }
    if (!crt) throw new Error('Secure Windows filesystem operations require a compatible CRT')
    native = { ...kernel32, ...ntdll, ...crt }
    return native
}

/** @param {number | bigint} value */
function asBigInt(value) {
    return typeof value === 'bigint' ? value : BigInt(value)
}

/** @param {number} descriptor */
function handleForDescriptor(descriptor) {
    const value = asBigInt(symbols()._get_osfhandle(descriptor))
    if (isInvalidHandle(value)) {
        throw new Error('Secure Windows filesystem operation received an invalid descriptor')
    }
    return value
}

/** @param {bigint} handle @param {number} flags */
function descriptorForHandle(handle, flags) {
    const descriptor = symbols()._open_osfhandle(handle, crtFlags(flags))
    if (descriptor < 0) {
        symbols().CloseHandle(handle)
        throw new Error('Windows CRT refused a securely opened filesystem handle')
    }
    return descriptor
}

/** @param {number} flags */
function crtFlags(flags) {
    let result = WINDOWS_NATIVE_CONSTANTS.O_BINARY
    if (flags & constants.O_RDWR) result |= 2
    else if (flags & constants.O_WRONLY) result |= 1
    return result
}

/** @param {string} value */
function wide(value) {
    return Buffer.from(`${value}\0`, 'utf16le')
}

/** @param {DataView} view @param {number} offset @param {number | bigint} value */
function setPointer(view, offset, value) {
    view.setBigUint64(offset, asBigInt(value), true)
}

/** @param {number} status @param {string} label */
function ntFailure(status, label) {
    const win32 = symbols().RtlNtStatusToDosError(status)
    const error = /** @type {NodeJS.ErrnoException} */ (
        new Error(
            `${label} failed closed (NTSTATUS 0x${(status >>> 0).toString(16)}, Win32 ${win32})`
        )
    )
    error.code = `WIN32_${win32}`
    error.errno = win32
    throw error
}

/** @param {number} status */
function isMissingStatus(status) {
    return /** @type {number[]} */ ([
        WINDOWS_NATIVE_CONSTANTS.STATUS_OBJECT_NAME_NOT_FOUND,
        WINDOWS_NATIVE_CONSTANTS.STATUS_OBJECT_PATH_NOT_FOUND,
        WINDOWS_NATIVE_CONSTANTS.STATUS_NO_SUCH_FILE
    ]).includes(status)
}

/**
 * Opens one name relative to a pinned directory. FILE_OPEN_REPARSE_POINT is
 * mandatory: a junction or symlink is returned as the reparse object itself,
 * and FILE_DIRECTORY_FILE/NON_DIRECTORY_FILE then rejects it fail-closed.
 * @param {number} parentFd
 * @param {string} name
 * @param {{ directory?: boolean, create?: boolean, flags?: number, mode?: number, deleteAccess?: boolean }} options
 */
function ntOpenRelative(parentFd, name, options = {}) {
    if (
        !name ||
        name === '.' ||
        name === '..' ||
        name.includes('\\') ||
        name.includes('/') ||
        name.includes('\0')
    ) {
        throw new Error(`Unsafe rooted path component: ${name}`)
    }
    const api = symbols()
    const encoded = wide(name)
    const unicode = Buffer.alloc(WINDOWS_NATIVE_LAYOUT.unicodeStringBytes)
    const unicodeView = new DataView(unicode.buffer, unicode.byteOffset, unicode.byteLength)
    const nameBytes = encoded.byteLength - 2
    unicodeView.setUint16(0, nameBytes, true)
    unicodeView.setUint16(2, nameBytes + 2, true)
    setPointer(unicodeView, 8, ptr(encoded))

    const attributes = Buffer.alloc(WINDOWS_NATIVE_LAYOUT.objectAttributesBytes)
    const attributesView = new DataView(
        attributes.buffer,
        attributes.byteOffset,
        attributes.byteLength
    )
    attributesView.setUint32(0, WINDOWS_NATIVE_LAYOUT.objectAttributesBytes, true)
    setPointer(attributesView, 8, handleForDescriptor(parentFd))
    setPointer(attributesView, 16, ptr(unicode))
    attributesView.setUint32(24, WINDOWS_NATIVE_CONSTANTS.OBJ_CASE_INSENSITIVE, true)

    const output = Buffer.alloc(8)
    const io = Buffer.alloc(WINDOWS_NATIVE_LAYOUT.ioStatusBlockBytes)
    const flags = options.flags ?? constants.O_RDONLY
    let access =
        WINDOWS_NATIVE_CONSTANTS.FILE_READ_ATTRIBUTES | WINDOWS_NATIVE_CONSTANTS.SYNCHRONIZE
    if (options.directory) access |= WINDOWS_NATIVE_CONSTANTS.FILE_READ_DATA
    else if (flags & constants.O_RDWR) {
        access |=
            WINDOWS_NATIVE_CONSTANTS.FILE_READ_DATA |
            WINDOWS_NATIVE_CONSTANTS.FILE_WRITE_DATA |
            WINDOWS_NATIVE_CONSTANTS.FILE_WRITE_ATTRIBUTES
    } else if (flags & constants.O_WRONLY) {
        access |=
            WINDOWS_NATIVE_CONSTANTS.FILE_WRITE_DATA |
            WINDOWS_NATIVE_CONSTANTS.FILE_WRITE_ATTRIBUTES
    } else access |= WINDOWS_NATIVE_CONSTANTS.FILE_READ_DATA
    if (options.deleteAccess) access |= WINDOWS_NATIVE_CONSTANTS.DELETE

    /** @type {number} */
    let disposition = WINDOWS_NATIVE_CONSTANTS.FILE_OPEN
    if (options.create && flags & constants.O_EXCL)
        disposition = WINDOWS_NATIVE_CONSTANTS.FILE_CREATE
    else if (options.create) disposition = WINDOWS_NATIVE_CONSTANTS.FILE_OPEN_IF
    let createOptions =
        WINDOWS_NATIVE_CONSTANTS.FILE_SYNCHRONOUS_IO_NONALERT |
        WINDOWS_NATIVE_CONSTANTS.FILE_OPEN_REPARSE_POINT
    if (options.directory === true) createOptions |= WINDOWS_NATIVE_CONSTANTS.FILE_DIRECTORY_FILE
    if (options.directory === false)
        createOptions |= WINDOWS_NATIVE_CONSTANTS.FILE_NON_DIRECTORY_FILE
    const status = api.NtCreateFile(
        ptr(output),
        access,
        ptr(attributes),
        ptr(io),
        null,
        options.directory
            ? WINDOWS_NATIVE_CONSTANTS.FILE_ATTRIBUTE_DIRECTORY
            : WINDOWS_NATIVE_CONSTANTS.FILE_ATTRIBUTE_NORMAL,
        WINDOWS_NATIVE_CONSTANTS.FILE_SHARE_READ |
            WINDOWS_NATIVE_CONSTANTS.FILE_SHARE_WRITE |
            WINDOWS_NATIVE_CONSTANTS.FILE_SHARE_DELETE,
        disposition,
        createOptions,
        null,
        0
    )
    if (status < 0) return { status, descriptor: -1 }
    const handle = read.u64(ptr(output), 0)
    return { status, descriptor: descriptorForHandle(asBigInt(handle), flags) }
}

/** @param {string} relative */
function safeParts(relative) {
    const parts = relative.split(/[\\/]/)
    if (
        !relative ||
        path.win32.isAbsolute(relative) ||
        parts.some((part) => !part || part === '.' || part === '..')
    ) {
        throw new Error(`Unsafe rooted path: ${relative}`)
    }
    return parts
}

/** @param {number} descriptor */
function syncDescriptor(descriptor) {
    try {
        fsyncSync(descriptor)
    } catch {
        // Windows does not allow FlushFileBuffers on every directory handle;
        // file handles are flushed by transaction callers before rename.
    }
}

/** @param {string} target */
export function windowsTryAcquireProcessFileLock(target) {
    assertSupportedRuntime()
    const parent = windowsOpenDirectoryNoFollow(path.win32.dirname(path.win32.resolve(target)))
    let descriptor
    try {
        const opened = ntOpenRelative(parent, path.win32.basename(target), {
            directory: false,
            create: true,
            flags: constants.O_CREAT | constants.O_RDWR
        })
        if (opened.status < 0) ntFailure(opened.status, `Secure lock sentinel open for ${target}`)
        descriptor = opened.descriptor
    } finally {
        closeSync(parent)
    }
    const overlapped = Buffer.alloc(WINDOWS_NATIVE_LAYOUT.overlappedBytes)
    const api = symbols()
    const handle = handleForDescriptor(descriptor)
    if (
        !api.LockFileEx(
            handle,
            WINDOWS_NATIVE_CONSTANTS.LOCKFILE_EXCLUSIVE_LOCK |
                WINDOWS_NATIVE_CONSTANTS.LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            ptr(overlapped)
        )
    ) {
        const code = api.GetLastError()
        closeSync(descriptor)
        if (code === WINDOWS_NATIVE_CONSTANTS.ERROR_LOCK_VIOLATION) return null
        throw new Error(`LockFileEx failed for ${target} (Win32 ${code})`)
    }
    let released = false
    return () => {
        if (released) return
        released = true
        try {
            api.UnlockFileEx(handle, 0, 1, 0, ptr(overlapped))
        } finally {
            closeSync(descriptor)
        }
    }
}

/** @param {string} target */
export function windowsOpenDirectoryNoFollow(target) {
    assertSupportedRuntime()
    const resolved = path.win32.resolve(target)
    const parsed = path.win32.parse(resolved)
    if (!/^[A-Za-z]:\\$/.test(parsed.root)) {
        throw new Error('Secure Windows traversal currently requires a local drive path')
    }
    const api = symbols()
    const rootName = wide(parsed.root)
    const rootHandle = asBigInt(
        api.CreateFileW(
            ptr(rootName),
            WINDOWS_NATIVE_CONSTANTS.FILE_READ_DATA |
                WINDOWS_NATIVE_CONSTANTS.FILE_READ_ATTRIBUTES |
                WINDOWS_NATIVE_CONSTANTS.SYNCHRONIZE,
            WINDOWS_NATIVE_CONSTANTS.FILE_SHARE_READ |
                WINDOWS_NATIVE_CONSTANTS.FILE_SHARE_WRITE |
                WINDOWS_NATIVE_CONSTANTS.FILE_SHARE_DELETE,
            null,
            WINDOWS_NATIVE_CONSTANTS.OPEN_EXISTING,
            WINDOWS_NATIVE_CONSTANTS.FILE_FLAG_BACKUP_SEMANTICS |
                WINDOWS_NATIVE_CONSTANTS.FILE_FLAG_OPEN_REPARSE_POINT,
            null
        )
    )
    if (isInvalidHandle(rootHandle)) {
        throw new Error(`CreateFileW failed for trusted volume root (Win32 ${api.GetLastError()})`)
    }
    let current = descriptorForHandle(rootHandle, constants.O_RDONLY)
    try {
        const suffix = resolved.slice(parsed.root.length)
        for (const component of safeParts(suffix)) {
            const opened = ntOpenRelative(current, component, { directory: true })
            if (opened.status < 0) ntFailure(opened.status, `Secure directory open for ${target}`)
            closeSync(current)
            current = opened.descriptor
        }
        const result = current
        current = -1
        return result
    } finally {
        if (current >= 0) closeSync(current)
    }
}

/** @param {number} rootFd @param {string} relative @param {boolean} strict */
function openAtRoot(rootFd, relative, strict) {
    const parts = safeParts(relative)
    let current = rootFd
    let ownsCurrent = false
    try {
        for (let index = 0; index < parts.length; index += 1) {
            const opened = ntOpenRelative(
                current,
                parts[index],
                index < parts.length - 1 ? { directory: true } : {}
            )
            if (opened.status < 0) {
                if (isMissingStatus(opened.status)) return null
                if (!strict) return null
                ntFailure(opened.status, `Secure rooted open for ${relative}`)
            }
            if (ownsCurrent) closeSync(current)
            current = opened.descriptor
            ownsCurrent = true
        }
        const result = current
        ownsCurrent = false
        return result
    } finally {
        if (ownsCurrent) closeSync(current)
    }
}

/** @param {number} rootFd @param {string} relative */
export function windowsOpenFileAtRoot(rootFd, relative) {
    return openAtRoot(rootFd, relative, false)
}

/** @param {number} rootFd @param {string} relative */
export function windowsOpenFileAtRootStrict(rootFd, relative) {
    return openAtRoot(rootFd, relative, true)
}

/**
 * Returns the kernel's current DOS path for a pinned descriptor. The result is
 * intended for Windows ADS access only; recovery mutations themselves remain
 * handle-relative.
 * @param {number} descriptor
 */
export function windowsPathForDescriptor(descriptor) {
    const api = symbols()
    const handle = handleForDescriptor(descriptor)
    const required = api.GetFinalPathNameByHandleW(handle, null, 0, 0)
    if (!required) {
        throw new Error(`GetFinalPathNameByHandleW failed (Win32 ${api.GetLastError()})`)
    }
    const buffer = Buffer.alloc((required + 1) * 2)
    const written = api.GetFinalPathNameByHandleW(handle, ptr(buffer), required + 1, 0)
    if (!written || written > required) {
        throw new Error(`GetFinalPathNameByHandleW failed (Win32 ${api.GetLastError()})`)
    }
    const nativePath = buffer.subarray(0, written * 2).toString('utf16le')
    if (nativePath.startsWith('\\\\?\\UNC\\')) return `\\\\${nativePath.slice(8)}`
    if (nativePath.startsWith('\\\\?\\')) return nativePath.slice(4)
    throw new Error('Windows returned a non-DOS descriptor path; ADS access failed closed')
}

/** @param {number} rootFd @param {string} relative @param {boolean} create */
function openParent(rootFd, relative, create) {
    const parts = safeParts(relative)
    const name = /** @type {string} */ (parts.pop())
    let current = rootFd
    let ownsCurrent = false
    try {
        for (const component of parts) {
            const opened = ntOpenRelative(current, component, { directory: true, create })
            if (opened.status < 0)
                ntFailure(opened.status, `Secure rooted parent open for ${relative}`)
            if (ownsCurrent) closeSync(current)
            current = opened.descriptor
            ownsCurrent = true
        }
        return { fd: current, ownsFd: ownsCurrent, name }
    } catch (error) {
        if (ownsCurrent) closeSync(current)
        throw error
    }
}

/** @param {number} rootFd @param {string} relative @param {number} flags @param {number} mode */
export function windowsOpenFileAtRootWithFlags(rootFd, relative, flags, mode = 0o600) {
    const parent = openParent(rootFd, relative, Boolean(flags & constants.O_CREAT))
    try {
        const opened = ntOpenRelative(parent.fd, parent.name, {
            directory: false,
            flags,
            create: Boolean(flags & constants.O_CREAT),
            mode
        })
        if (opened.status < 0) ntFailure(opened.status, `Secure rooted file open for ${relative}`)
        return opened.descriptor
    } finally {
        if (parent.ownsFd) closeSync(parent.fd)
    }
}

/** @param {number} rootFd @param {string} relative @param {boolean} directory */
export function windowsUnlinkAtRoot(rootFd, relative, directory = false) {
    const parent = openParent(rootFd, relative, false)
    let descriptor = -1
    try {
        const opened = ntOpenRelative(parent.fd, parent.name, { directory, deleteAccess: true })
        if (isMissingStatus(opened.status)) return
        if (opened.status < 0) ntFailure(opened.status, `Secure rooted delete open for ${relative}`)
        descriptor = opened.descriptor
        const disposition = Buffer.alloc(4)
        disposition.writeUInt32LE(
            WINDOWS_NATIVE_CONSTANTS.FILE_DISPOSITION_FLAG_DELETE |
                WINDOWS_NATIVE_CONSTANTS.FILE_DISPOSITION_FLAG_POSIX_SEMANTICS |
                WINDOWS_NATIVE_CONSTANTS.FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE
        )
        if (
            !symbols().SetFileInformationByHandle(
                handleForDescriptor(descriptor),
                WINDOWS_NATIVE_CONSTANTS.FILE_DISPOSITION_INFO_EX,
                ptr(disposition),
                disposition.byteLength
            )
        ) {
            throw new Error(
                `Secure rooted delete failed for ${relative} (Win32 ${symbols().GetLastError()})`
            )
        }
        syncDescriptor(parent.fd)
    } finally {
        if (descriptor >= 0) closeSync(descriptor)
        if (parent.ownsFd) closeSync(parent.fd)
    }
}

/** @param {number} sourceRootFd @param {string} sourceRelative @param {number} targetRootFd @param {string} targetRelative */
export function windowsRenameAtRoots(sourceRootFd, sourceRelative, targetRootFd, targetRelative) {
    const source = openParent(sourceRootFd, sourceRelative, false)
    const target = openParent(targetRootFd, targetRelative, true)
    let descriptor = -1
    try {
        const opened = ntOpenRelative(source.fd, source.name, { deleteAccess: true })
        if (opened.status < 0)
            ntFailure(opened.status, `Secure rooted rename open for ${sourceRelative}`)
        descriptor = opened.descriptor
        const encodedName = Buffer.from(target.name, 'utf16le')
        const info = Buffer.alloc(
            WINDOWS_NATIVE_LAYOUT.fileRenameHeaderBytes + encodedName.byteLength
        )
        const view = new DataView(info.buffer, info.byteOffset, info.byteLength)
        view.setUint32(
            0,
            WINDOWS_NATIVE_CONSTANTS.FILE_RENAME_FLAG_REPLACE_IF_EXISTS |
                WINDOWS_NATIVE_CONSTANTS.FILE_RENAME_FLAG_POSIX_SEMANTICS,
            true
        )
        setPointer(view, 8, handleForDescriptor(target.fd))
        view.setUint32(16, encodedName.byteLength, true)
        encodedName.copy(info, WINDOWS_NATIVE_LAYOUT.fileRenameHeaderBytes)
        if (
            !symbols().SetFileInformationByHandle(
                handleForDescriptor(descriptor),
                WINDOWS_NATIVE_CONSTANTS.FILE_RENAME_INFO_EX,
                ptr(info),
                info.byteLength
            )
        ) {
            throw new Error(
                `Secure rooted rename failed for ${sourceRelative} (Win32 ${symbols().GetLastError()})`
            )
        }
        syncDescriptor(target.fd)
        if (source.fd !== target.fd) syncDescriptor(source.fd)
    } finally {
        if (descriptor >= 0) closeSync(descriptor)
        if (source.ownsFd) closeSync(source.fd)
        if (target.ownsFd) closeSync(target.fd)
    }
}
