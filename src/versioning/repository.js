import { createHash } from 'node:crypto'
import { cp, mkdir, readFile, readdir, rm, stat } from 'node:fs/promises'
import { existsSync, readFileSync } from 'node:fs'
import path from 'node:path'
import TTID from '@d31ma/ttid'
import { writeDurable } from '../storage/durable.js'
import { FilesystemEngine } from '../storage/engine.js'

const DEFAULT_BRANCH = 'main'
const METADATA_DIR = '.fylo-vcs'
const COLLECTIONS_DIR = '.collections'

/**
 * @typedef {object} FyloBranchRef
 * @property {string} name
 * @property {string | null} head
 * @property {string} root
 * @property {string} createdAt
 * @property {string} updatedAt
 * @property {string | undefined} sourceBranch
 * @property {string | null | undefined} sourceCommit
 */

/**
 * @typedef {object} FyloCommitManifest
 * @property {string} id
 * @property {string} branch
 * @property {string[]} parents
 * @property {string} message
 * @property {string} createdAt
 * @property {string} root
 */

/**
 * @typedef {object} FyloCheckoutResult
 * @property {string} branch
 * @property {boolean} created
 * @property {string | null} head
 * @property {string} root
 */

/**
 * @typedef {object} FyloBranchListResult
 * @property {string} current
 * @property {FyloBranchRef[]} branches
 */

/**
 * @typedef {'active' | 'deleted'} FyloVersionedDocumentKind
 */

/**
 * @typedef {'added' | 'modified' | 'deleted'} FyloTreeChangeStatus
 */

/**
 * @typedef {object} FyloTreeChange
 * @property {FyloTreeChangeStatus} status
 * @property {string} collection
 * @property {FyloVersionedDocumentKind} kind
 * @property {string} id
 * @property {string} path
 */

/**
 * @typedef {object} FyloDiffResult
 * @property {string} from
 * @property {string} to
 * @property {{ added: number, modified: number, deleted: number, total: number }} counts
 * @property {FyloTreeChange[]} changes
 */

/**
 * @typedef {object} FyloStatusResult
 * @property {string} branch
 * @property {string | null} head
 * @property {boolean} clean
 * @property {FyloDiffResult} diff
 */

/**
 * @typedef {object} FyloRestoreCommitResult
 * @property {string} branch
 * @property {string} head
 * @property {string} restored
 * @property {boolean} forced
 * @property {string} root
 */

/**
 * @typedef {'already-up-to-date' | 'fast-forward' | 'merge' | 'conflict'} FyloMergeMode
 */

/**
 * @typedef {object} FyloMergeConflict
 * @property {string} collection
 * @property {FyloVersionedDocumentKind} kind
 * @property {string} id
 * @property {string} path
 * @property {string | null} baseHash
 * @property {string | null} oursHash
 * @property {string | null} theirsHash
 */

/**
 * @typedef {object} FyloMergeResult
 * @property {string} branch
 * @property {string} source
 * @property {string | null} base
 * @property {string | null} head
 * @property {FyloMergeMode} mode
 * @property {boolean} merged
 * @property {string[]} parents
 * @property {string | undefined} commit
 * @property {number} applied
 * @property {FyloMergeConflict[]} conflicts
 */

/**
 * File-backed repository metadata for FYLO document version control.
 *
 * This layer intentionally versions whole collection trees rather than diffs.
 * It is larger than copy-on-write storage, but it keeps recovery simple and
 * mirrors S3's full-object version semantics for the first production slice.
 */
export class VersionRepository {
    /** @type {string} */
    root

    /**
     * @param {string} root
     */
    constructor(root) {
        this.root = root
    }

    /** @returns {string} */
    metadataRoot() {
        return path.join(this.root, METADATA_DIR)
    }

    /** @returns {string} */
    refsRoot() {
        return path.join(this.metadataRoot(), 'refs', 'heads')
    }

    /** @returns {string} */
    commitsRoot() {
        return path.join(this.metadataRoot(), 'commits')
    }

    /** @returns {string} */
    hiddenBranchesRoot() {
        return path.join(this.metadataRoot(), 'branches')
    }

    /** @returns {string} */
    headPath() {
        return path.join(this.metadataRoot(), 'HEAD')
    }

    /**
     * @param {string} branch
     * @returns {string}
     */
    refPath(branch) {
        const target = path.join(this.refsRoot(), `${branch}.json`)
        assertPathInside(this.refsRoot(), target)
        return target
    }

    /**
     * @param {string} branch
     * @returns {string}
     */
    branchRoot(branch) {
        validateBranchName(branch)
        if (branch === DEFAULT_BRANCH) return this.root
        const target = path.join(this.hiddenBranchesRoot(), branch)
        assertPathInside(this.hiddenBranchesRoot(), target)
        return target
    }

    /**
     * @param {string} commitId
     * @returns {string}
     */
    commitRoot(commitId) {
        validateCommitId(commitId)
        return path.join(this.commitsRoot(), commitId)
    }

    /**
     * @returns {Promise<void>}
     */
    async init() {
        await mkdir(this.refsRoot(), { recursive: true })
        await mkdir(this.commitsRoot(), { recursive: true })
        await mkdir(this.hiddenBranchesRoot(), { recursive: true })
        if (!(await exists(this.refPath(DEFAULT_BRANCH)))) {
            await this.writeRef({
                name: DEFAULT_BRANCH,
                head: null,
                root: '.',
                createdAt: new Date().toISOString(),
                updatedAt: new Date().toISOString(),
                sourceBranch: undefined,
                sourceCommit: undefined
            })
        }
        if (!(await exists(this.headPath()))) await this.writeHead(DEFAULT_BRANCH)
    }

    /**
     * @param {string} branch
     * @param {{ create?: boolean }} [options]
     * @returns {Promise<FyloCheckoutResult>}
     */
    async checkout(branch, options = {}) {
        validateBranchName(branch)
        await this.init()
        const current = await this.currentBranch()
        if (options.create) {
            if (await exists(this.refPath(branch)))
                throw new Error(`Branch already exists: ${branch}`)
            const currentRef = await this.readRef(current)
            const branchRoot = this.branchRoot(branch)
            await mkdir(branchRoot, { recursive: true })
            await copyCollections(this.branchRoot(current), branchRoot)
            const now = new Date().toISOString()
            await this.writeRef({
                name: branch,
                head: currentRef.head,
                root: path.relative(this.root, branchRoot) || '.',
                createdAt: now,
                updatedAt: now,
                sourceBranch: current,
                sourceCommit: currentRef.head
            })
        } else if (!(await exists(this.refPath(branch)))) {
            throw new Error(`Branch not found: ${branch}`)
        }
        await this.writeHead(branch)
        const ref = await this.readRef(branch)
        return {
            branch,
            created: options.create === true,
            head: ref.head,
            root: this.branchRoot(branch)
        }
    }

    /**
     * @returns {Promise<string>}
     */
    async currentBranch() {
        await this.init()
        const head = (await readFile(this.headPath(), 'utf8')).trim()
        const match = head.match(/^ref: refs\/heads\/(.+)$/)
        if (!match) throw new Error('FYLO repository HEAD is corrupt')
        validateBranchName(match[1])
        return match[1]
    }

    /**
     * @returns {Promise<FyloBranchListResult>}
     */
    async listBranches() {
        await this.init()
        const branches = []
        for (const file of await listJsonFiles(this.refsRoot())) {
            const branch = file.slice(0, -'.json'.length)
            branches.push(await this.readRef(branch))
        }
        branches.sort((left, right) => left.name.localeCompare(right.name))
        return { current: await this.currentBranch(), branches }
    }

    /**
     * @param {string} message
     * @returns {Promise<FyloCommitManifest>}
     */
    async commit(message) {
        if (typeof message !== 'string' || message.trim().length === 0) {
            throw new Error('Commit message is required')
        }
        await this.init()
        const branch = await this.currentBranch()
        const ref = await this.readRef(branch)
        return await this.createCommit(branch, ref, ref.head ? [ref.head] : [], message.trim())
    }

    /**
     * @param {string} branch
     * @param {FyloBranchRef} ref
     * @param {string[]} parents
     * @param {string} message
     * @returns {Promise<FyloCommitManifest>}
     */
    async createCommit(branch, ref, parents, message) {
        const commitId = String(TTID.generate())
        const commitRoot = this.commitRoot(commitId)
        await mkdir(commitRoot, { recursive: true })
        await copyCollections(this.branchRoot(branch), commitRoot)
        const manifest = {
            id: commitId,
            branch,
            parents,
            message,
            createdAt: new Date().toISOString(),
            root: path.relative(this.root, commitRoot)
        }
        await writeDurable(
            path.join(commitRoot, 'manifest.json'),
            `${JSON.stringify(manifest, null, 2)}\n`
        )
        await this.writeRef({ ...ref, head: commitId, updatedAt: manifest.createdAt })
        return manifest
    }

    /**
     * @param {{ branch?: string, limit?: number }} [options]
     * @returns {Promise<FyloCommitManifest[]>}
     */
    async log(options = {}) {
        await this.init()
        const branch = options.branch ?? (await this.currentBranch())
        const ref = await this.readRef(branch)
        const commits = []
        let next = ref.head
        const limit = options.limit ?? 50
        while (next && commits.length < limit) {
            const manifest = await this.readCommit(next)
            commits.push(manifest)
            next = manifest.parents[0]
        }
        return commits
    }

    /**
     * @returns {Promise<FyloStatusResult>}
     */
    async status() {
        await this.init()
        const branch = await this.currentBranch()
        const ref = await this.readRef(branch)
        const diff = await this.diff('HEAD', 'WORKTREE')
        return { branch, head: ref.head, clean: diff.counts.total === 0, diff }
    }

    /**
     * @param {string} [from]
     * @param {string} [to]
     * @returns {Promise<FyloDiffResult>}
     */
    async diff(from = 'HEAD', to = 'WORKTREE') {
        await this.init()
        const [left, right] = await Promise.all([this.resolveTree(from), this.resolveTree(to)])
        const changes = await diffTrees(left.root, right.root)
        return {
            from: left.label,
            to: right.label,
            counts: countChanges(changes),
            changes
        }
    }

    /**
     * Restores a commit snapshot into the current branch working tree. The
     * commit objects remain immutable; this only moves the branch head and
     * replaces the branch's `.collections` tree.
     *
     * @param {string} commitId
     * @param {{ force?: boolean }} [options]
     * @returns {Promise<FyloRestoreCommitResult>}
     */
    async restoreCommit(commitId, options = {}) {
        validateCommitId(commitId)
        await this.init()
        await this.readCommit(commitId)
        const branch = await this.currentBranch()
        const ref = await this.readRef(branch)
        if (!options.force) {
            const status = await this.status()
            if (!status.clean) {
                throw new Error(
                    'Working tree has uncommitted changes; commit them first or pass --force'
                )
            }
        }
        const branchRoot = this.branchRoot(branch)
        await copyCollections(this.commitRoot(commitId), branchRoot)
        await this.writeRef({
            ...ref,
            head: commitId,
            updatedAt: new Date().toISOString()
        })
        return {
            branch,
            head: commitId,
            restored: commitId,
            forced: options.force === true,
            root: branchRoot
        }
    }

    /**
     * Merges a committed source snapshot into the current branch. The current
     * working tree must be clean so conflict detection always compares two
     * committed parents and never overwrites local uncommitted writes.
     *
     * @param {string} source
     * @param {{ message?: string }} [options]
     * @returns {Promise<FyloMergeResult>}
     */
    async merge(source, options = {}) {
        await this.init()
        const status = await this.status()
        if (!status.clean) {
            throw new Error('Working tree has uncommitted changes; commit them before merging')
        }
        const branch = await this.currentBranch()
        const ref = await this.readRef(branch)
        const theirs = await this.resolveCommit(source)
        const oursHead = ref.head
        if (oursHead === theirs.id) {
            return mergeResult(branch, theirs.id, theirs.id, theirs.id, 'already-up-to-date', true)
        }
        if (await this.isAncestor(theirs.id, oursHead)) {
            return mergeResult(branch, theirs.id, theirs.id, oursHead, 'already-up-to-date', true)
        }
        if (!oursHead || (await this.isAncestor(oursHead, theirs.id))) {
            await copyCollections(this.commitRoot(theirs.id), this.branchRoot(branch))
            await this.writeRef({ ...ref, head: theirs.id, updatedAt: new Date().toISOString() })
            return mergeResult(branch, theirs.id, oursHead, theirs.id, 'fast-forward', true)
        }
        const base = await this.commonAncestor(oursHead, theirs.id)
        const plan = await planThreeWayMerge(
            base ? this.commitRoot(base) : '',
            this.commitRoot(oursHead),
            this.commitRoot(theirs.id)
        )
        if (plan.conflicts.length > 0) {
            return {
                branch,
                source: theirs.id,
                base,
                head: oursHead,
                mode: 'conflict',
                merged: false,
                parents: [oursHead, theirs.id],
                commit: undefined,
                applied: 0,
                conflicts: plan.conflicts
            }
        }
        for (const change of plan.apply)
            await applyTreeChange(this.branchRoot(branch), this.commitRoot(theirs.id), change)
        await rebuildChangedCollections(this.branchRoot(branch), plan.apply)
        const message = options.message?.trim() || `Merge ${source} into ${branch}`
        const commit = await this.createCommit(branch, ref, [oursHead, theirs.id], message)
        return {
            branch,
            source: theirs.id,
            base,
            head: commit.id,
            mode: 'merge',
            merged: true,
            parents: commit.parents,
            commit: commit.id,
            applied: plan.apply.length,
            conflicts: []
        }
    }

    /**
     * @param {string} commitId
     * @returns {Promise<FyloCommitManifest>}
     */
    async readCommit(commitId) {
        validateCommitId(commitId)
        const text = await readFile(path.join(this.commitRoot(commitId), 'manifest.json'), 'utf8')
        return /** @type {FyloCommitManifest} */ (JSON.parse(text))
    }

    /**
     * @param {string} branch
     * @returns {Promise<FyloBranchRef>}
     */
    async readRef(branch) {
        validateBranchName(branch)
        return /** @type {FyloBranchRef} */ (
            JSON.parse(await readFile(this.refPath(branch), 'utf8'))
        )
    }

    /**
     * @param {string} ref
     * @returns {Promise<{ label: string, root: string }>}
     */
    async resolveTree(ref) {
        const normalized = ref.trim()
        if (normalized === 'WORKTREE') {
            const branch = await this.currentBranch()
            return { label: `${branch}:WORKTREE`, root: this.branchRoot(branch) }
        }
        if (normalized === 'HEAD') {
            const branch = await this.currentBranch()
            const branchRef = await this.readRef(branch)
            if (!branchRef.head) return { label: `${branch}:HEAD`, root: '' }
            return { label: `${branch}:HEAD`, root: this.commitRoot(branchRef.head) }
        }
        if (TTID.isTTID(normalized)) {
            await this.readCommit(normalized)
            return { label: normalized, root: this.commitRoot(normalized) }
        }
        validateBranchName(normalized)
        const branchRef = await this.readRef(normalized)
        return { label: normalized, root: this.branchRoot(branchRef.name) }
    }

    /**
     * @param {string} ref
     * @returns {Promise<FyloCommitManifest>}
     */
    async resolveCommit(ref) {
        const normalized = ref.trim()
        if (normalized === 'HEAD') {
            const branch = await this.currentBranch()
            const branchRef = await this.readRef(branch)
            if (!branchRef.head) throw new Error(`Branch has no commits: ${branch}`)
            return await this.readCommit(branchRef.head)
        }
        if (TTID.isTTID(normalized)) return await this.readCommit(normalized)
        validateBranchName(normalized)
        const branchRef = await this.readRef(normalized)
        if (!branchRef.head) throw new Error(`Branch has no commits: ${normalized}`)
        return await this.readCommit(branchRef.head)
    }

    /**
     * @param {string} ancestor
     * @param {string | null} descendant
     * @returns {Promise<boolean>}
     */
    async isAncestor(ancestor, descendant) {
        if (!descendant) return false
        return (await this.ancestorDepths(descendant)).has(ancestor)
    }

    /**
     * @param {string} left
     * @param {string} right
     * @returns {Promise<string | null>}
     */
    async commonAncestor(left, right) {
        const [leftAncestors, rightAncestors] = await Promise.all([
            this.ancestorDepths(left),
            this.ancestorDepths(right)
        ])
        let best = null
        let bestDistance = Number.POSITIVE_INFINITY
        for (const [commitId, leftDepth] of leftAncestors) {
            const rightDepth = rightAncestors.get(commitId)
            if (rightDepth === undefined) continue
            const distance = leftDepth + rightDepth
            if (distance < bestDistance) {
                best = commitId
                bestDistance = distance
            }
        }
        return best
    }

    /**
     * @param {string} commitId
     * @returns {Promise<Map<string, number>>}
     */
    async ancestorDepths(commitId) {
        /** @type {Map<string, number>} */
        const ancestors = new Map()
        /** @type {{ id: string, depth: number }[]} */
        const queue = [{ id: commitId, depth: 0 }]
        while (queue.length > 0) {
            const next = /** @type {{ id: string, depth: number }} */ (queue.shift())
            if (ancestors.has(next.id)) continue
            ancestors.set(next.id, next.depth)
            const commit = await this.readCommit(next.id)
            for (const parent of commit.parents) queue.push({ id: parent, depth: next.depth + 1 })
        }
        return ancestors
    }

    /**
     * @param {FyloBranchRef} ref
     * @returns {Promise<void>}
     */
    async writeRef(ref) {
        validateBranchName(ref.name)
        await writeDurable(this.refPath(ref.name), `${JSON.stringify(ref, null, 2)}\n`)
    }

    /**
     * @param {string} branch
     * @returns {Promise<void>}
     */
    async writeHead(branch) {
        validateBranchName(branch)
        await writeDurable(this.headPath(), `ref: refs/heads/${branch}\n`)
    }

    /**
     * Resolves the storage root for regular FYLO operations. When no repository
     * metadata exists, the caller keeps using the original root unchanged.
     *
     * @param {string} root
     * @returns {string}
     */
    static resolveActiveRoot(root) {
        const headPath = path.join(root, METADATA_DIR, 'HEAD')
        if (!existsSync(headPath)) return root
        const head = readFileSync(headPath, 'utf8').trim()
        const match = head.match(/^ref: refs\/heads\/(.+)$/)
        if (!match) throw new Error('FYLO repository HEAD is corrupt')
        const branch = match[1]
        validateBranchName(branch)
        if (branch === DEFAULT_BRANCH) return root
        const branchRoot = path.join(root, METADATA_DIR, 'branches', branch)
        assertPathInside(path.join(root, METADATA_DIR, 'branches'), branchRoot)
        if (!existsSync(path.join(root, METADATA_DIR, 'refs', 'heads', `${branch}.json`))) {
            throw new Error(`Active FYLO branch is missing its ref: ${branch}`)
        }
        return branchRoot
    }
}

/**
 * @param {string} name
 * @returns {void}
 */
export function validateBranchName(name) {
    if (typeof name !== 'string' || name.length === 0 || name.length > 128) {
        throw new Error('Branch name must be a non-empty string up to 128 characters')
    }
    if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(name)) {
        throw new Error('Branch name may contain letters, numbers, ".", "_", "-", and "/"')
    }
    if (
        name.includes('..') ||
        name.includes('//') ||
        name.endsWith('/') ||
        name.endsWith('.lock')
    ) {
        throw new Error(`Invalid branch name: ${name}`)
    }
}

/**
 * @param {string} commitId
 * @returns {void}
 */
function validateCommitId(commitId) {
    if (typeof commitId !== 'string' || !TTID.isTTID(commitId)) {
        throw new Error(`Invalid commit id: ${commitId}`)
    }
}

/**
 * @param {string} root
 * @returns {Promise<string[]>}
 */
async function listJsonFiles(root) {
    /** @type {string[]} */
    const files = []
    /**
     * @param {string} directory
     * @param {string} [prefix]
     * @returns {Promise<void>}
     */
    async function walk(directory, prefix = '') {
        const entries = await readdir(directory, { withFileTypes: true })
        for (const entry of entries) {
            const relative = prefix ? `${prefix}/${entry.name}` : entry.name
            const full = path.join(directory, entry.name)
            if (entry.isDirectory()) {
                await walk(full, relative)
                continue
            }
            if (entry.isFile() && relative.endsWith('.json')) files.push(relative)
        }
    }
    await walk(root)
    return files
}

/**
 * @param {string} sourceRoot
 * @param {string} targetRoot
 * @returns {Promise<void>}
 */
async function copyCollections(sourceRoot, targetRoot) {
    const source = path.join(sourceRoot, COLLECTIONS_DIR)
    const target = path.join(targetRoot, COLLECTIONS_DIR)
    await rm(target, { recursive: true, force: true })
    try {
        const sourceStats = await stat(source)
        if (!sourceStats.isDirectory()) return
        await cp(source, target, { recursive: true, preserveTimestamps: true })
    } catch (error) {
        if (/** @type {NodeJS.ErrnoException} */ (error).code !== 'ENOENT') throw error
    }
}

/**
 * @param {string} leftRoot
 * @param {string} rightRoot
 * @returns {Promise<FyloTreeChange[]>}
 */
async function diffTrees(leftRoot, rightRoot) {
    const [left, right] = await Promise.all([
        readDocumentTree(leftRoot),
        readDocumentTree(rightRoot)
    ])
    const keys = [...new Set([...left.keys(), ...right.keys()])].sort()
    /** @type {FyloTreeChange[]} */
    const changes = []
    for (const key of keys) {
        const leftEntry = left.get(key)
        const rightEntry = right.get(key)
        if (!leftEntry && rightEntry) {
            changes.push({ ...rightEntry, status: 'added' })
            continue
        }
        if (leftEntry && !rightEntry) {
            changes.push({ ...leftEntry, status: 'deleted' })
            continue
        }
        if (leftEntry && rightEntry && leftEntry.hash !== rightEntry.hash) {
            changes.push({ ...rightEntry, status: 'modified' })
        }
    }
    return changes
}

/**
 * @typedef {object} ThreeWayMergePlan
 * @property {(DocumentTreeEntry & { deleted?: boolean })[]} apply
 * @property {FyloMergeConflict[]} conflicts
 */

/**
 * @param {string} baseRoot
 * @param {string} oursRoot
 * @param {string} theirsRoot
 * @returns {Promise<ThreeWayMergePlan>}
 */
async function planThreeWayMerge(baseRoot, oursRoot, theirsRoot) {
    const [base, ours, theirs] = await Promise.all([
        readDocumentTree(baseRoot),
        readDocumentTree(oursRoot),
        readDocumentTree(theirsRoot)
    ])
    const keys = [...new Set([...base.keys(), ...ours.keys(), ...theirs.keys()])].sort()
    /** @type {(DocumentTreeEntry & { deleted?: boolean })[]} */
    const apply = []
    /** @type {FyloMergeConflict[]} */
    const conflicts = []
    for (const key of keys) {
        const baseEntry = base.get(key)
        const oursEntry = ours.get(key)
        const theirsEntry = theirs.get(key)
        const baseHash = baseEntry?.hash ?? null
        const oursHash = oursEntry?.hash ?? null
        const theirsHash = theirsEntry?.hash ?? null
        if (oursHash === theirsHash) continue
        if (baseHash === theirsHash) continue
        if (baseHash === oursHash) {
            if (theirsEntry) apply.push(theirsEntry)
            else {
                const deletionEntry = oursEntry ?? baseEntry
                if (deletionEntry) apply.push({ ...deletionEntry, hash: '', deleted: true })
            }
            continue
        }
        const representative = theirsEntry ?? oursEntry ?? baseEntry
        if (!representative) continue
        conflicts.push({
            collection: representative.collection,
            kind: representative.kind,
            id: representative.id,
            path: representative.path,
            baseHash,
            oursHash,
            theirsHash
        })
    }
    return { apply, conflicts }
}

/**
 * @param {string} targetRoot
 * @param {string} sourceRoot
 * @param {DocumentTreeEntry & { deleted?: boolean }} change
 * @returns {Promise<void>}
 */
async function applyTreeChange(targetRoot, sourceRoot, change) {
    const target = path.join(targetRoot, change.path)
    assertPathInside(targetRoot, target)
    if (change.deleted) {
        await rm(target, { force: true })
        return
    }
    const source = path.join(sourceRoot, change.path)
    assertPathInside(sourceRoot, source)
    await mkdir(path.dirname(target), { recursive: true })
    await cp(source, target, { preserveTimestamps: true })
}

/**
 * @param {string} root
 * @param {(DocumentTreeEntry & { deleted?: boolean })[]} changes
 * @returns {Promise<void>}
 */
async function rebuildChangedCollections(root, changes) {
    const collections = [...new Set(changes.map((change) => change.collection))]
    if (collections.length === 0) return
    const engine = new FilesystemEngine(root)
    for (const collection of collections) await engine.rebuildCollection(collection)
}

/**
 * @param {string} branch
 * @param {string} source
 * @param {string | null} base
 * @param {string | null} head
 * @param {FyloMergeMode} mode
 * @param {boolean} merged
 * @returns {FyloMergeResult}
 */
function mergeResult(branch, source, base, head, mode, merged) {
    return {
        branch,
        source,
        base,
        head,
        mode,
        merged,
        parents: head && head !== source ? [head, source] : [source],
        commit: undefined,
        applied: 0,
        conflicts: []
    }
}

/**
 * @param {FyloTreeChange[]} changes
 * @returns {{ added: number, modified: number, deleted: number, total: number }}
 */
function countChanges(changes) {
    const counts = { added: 0, modified: 0, deleted: 0, total: changes.length }
    for (const change of changes) counts[change.status]++
    return counts
}

/**
 * @typedef {object} DocumentTreeEntry
 * @property {string} collection
 * @property {FyloVersionedDocumentKind} kind
 * @property {string} id
 * @property {string} path
 * @property {string} hash
 */

/**
 * @param {string} root
 * @returns {Promise<Map<string, DocumentTreeEntry>>}
 */
async function readDocumentTree(root) {
    const entries = new Map()
    if (!root) return entries
    const collectionsRoot = path.join(root, COLLECTIONS_DIR)
    if (!(await exists(collectionsRoot))) return entries
    for (const collectionEntry of await readdir(collectionsRoot, { withFileTypes: true })) {
        if (!collectionEntry.isDirectory()) continue
        const collection = collectionEntry.name
        const collectionRoot = path.join(collectionsRoot, collection)
        await readDocumentNamespace(entries, collectionRoot, collection, 'docs', 'active')
        await readDocumentNamespace(entries, collectionRoot, collection, '.deleted', 'deleted')
    }
    return entries
}

/**
 * @param {Map<string, DocumentTreeEntry>} entries
 * @param {string} collectionRoot
 * @param {string} collection
 * @param {string} namespace
 * @param {FyloVersionedDocumentKind} kind
 * @returns {Promise<void>}
 */
async function readDocumentNamespace(entries, collectionRoot, collection, namespace, kind) {
    const namespaceRoot = path.join(collectionRoot, namespace)
    if (!(await exists(namespaceRoot))) return
    for (const file of await listJsonFiles(namespaceRoot)) {
        const id = path.basename(file, '.json')
        const relativePath = path.join(COLLECTIONS_DIR, collection, namespace, file)
        const hash = await hashFile(path.join(namespaceRoot, file))
        entries.set(`${collection}/${kind}/${id}`, {
            collection,
            kind,
            id,
            path: relativePath,
            hash
        })
    }
}

/**
 * @param {string} target
 * @returns {Promise<string>}
 */
async function hashFile(target) {
    return createHash('sha256')
        .update(await readFile(target))
        .digest('hex')
}

/**
 * @param {string} target
 * @returns {Promise<boolean>}
 */
async function exists(target) {
    try {
        await stat(target)
        return true
    } catch (error) {
        if (/** @type {NodeJS.ErrnoException} */ (error).code === 'ENOENT') return false
        throw error
    }
}

/**
 * @param {string} root
 * @param {string} target
 * @returns {void}
 */
function assertPathInside(root, target) {
    const relative = path.relative(path.resolve(root), path.resolve(target))
    if (relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative))) return
    throw new Error(`Path escapes FYLO repository metadata: ${target}`)
}
