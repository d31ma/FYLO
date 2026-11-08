export class BrowserPath {
    static #separator = '/'
    static #currentDirectory = '.'
    static #parentDirectory = '..'
    static #emptyPath = ''
    static #stringType = 'string'
    static #unsafePathMessage = 'Unsafe document path: '
    static #emptyLength = 0
    static #firstIndex = 0

    /** @param {string} value @returns {string} */
    static normalize(value) {
        return this.#normalizeInput(value)
    }

    /** @param {...(string | undefined | null)} segments @returns {string} */
    static join(...segments) {
        const strings = this.#stringParts(segments)
        const parts = this.#withoutEmpty(strings)
        return this.normalize(parts.join(this.#separator))
    }

    /** @param {string} value @returns {string} */
    static dirname(value) {
        const normalized = this.normalize(value)
        const index = normalized.lastIndexOf(this.#separator)
        const parent = normalized.slice(0, this.#parentEnd(index))
        return this.#parentOrDot(parent, index)
    }

    /** @param {string} value @returns {string} */
    static basename(value) {
        const normalized = this.normalize(value)
        return normalized.slice(normalized.lastIndexOf(this.#separator) + 1)
    }

    /** @param {string} from @param {string} value @returns {string} */
    static resolve(from, value) {
        const target = this.#resolutionTarget(from, value)
        return this.normalize(this.#resolutionInput(from, target))
    }

    /** @param {string} from @param {string} to @returns {string} */
    static relative(from, to) {
        const fromSegments = this.#pathSegments(from)
        const toSegments = this.#pathSegments(to)
        const common = this.#commonLength(fromSegments, toSegments)
        const parents = Array(fromSegments.length - common).fill(this.#parentDirectory)
        return [...parents, ...toSegments.slice(common)].join(this.#separator)
    }

    /** @param {string} parent @param {string} target */
    static assertPathInside(parent, target) {
        const normalizedParent = this.normalize(parent)
        const normalizedTarget = this.normalize(target)
        this.#checkDifferentPaths(normalizedParent, normalizedTarget, target)
    }

    /** @param {string} value @returns {string} */
    static #normalizeInput(value) {
        if (typeof value !== this.#stringType) return this.#emptyPath
        return this.#normalizeString(value)
    }

    /** @param {string} value @returns {string} */
    static #normalizeString(value) {
        const absolute = value.startsWith(this.#separator)
        const parts = this.#resolveSegments(value, absolute)
        const joined = parts.join(this.#separator)
        const relative = this.#relativeOrDot(joined)
        return this.#rootedPath(joined, relative, absolute)
    }

    /** @param {string} value @param {boolean} absolute @returns {string[]} */
    static #resolveSegments(value, absolute) {
        /** @type {string[]} */
        const parts = []
        for (const segment of value.split(this.#separator)) {
            this.#appendSegment(parts, segment, absolute)
        }
        return parts
    }

    /** @param {string[]} parts @param {string} segment @param {boolean} absolute */
    static #appendSegment(parts, segment, absolute) {
        switch (segment) {
            case this.#emptyPath:
            case this.#currentDirectory:
                break
            case this.#parentDirectory:
                this.#resolveParent(parts, absolute)
                break
            default:
                parts.push(segment)
        }
    }

    /** @param {string[]} parts @param {boolean} absolute */
    static #resolveParent(parts, absolute) {
        const canRemove = this.#canRemoveParent(parts)
        this.#removeParent(parts, canRemove)
        this.#appendParent(parts, this.#needsParent(canRemove, absolute))
    }

    /** @param {string[]} parts @returns {boolean} */
    static #canRemoveParent(parts) {
        return parts.length > this.#emptyLength && parts[parts.length - 1] !== this.#parentDirectory
    }

    /** @param {boolean} canRemove @param {boolean} absolute @returns {boolean} */
    static #needsParent(canRemove, absolute) {
        return !canRemove && !absolute
    }

    /** @param {string[]} parts @param {boolean} canRemove */
    static #removeParent(parts, canRemove) {
        if (canRemove) parts.pop()
    }

    /** @param {string[]} parts @param {boolean} needed */
    static #appendParent(parts, needed) {
        if (needed) parts.push(this.#parentDirectory)
    }

    /** @param {(string | undefined | null)[]} segments @returns {string[]} */
    static #stringParts(segments) {
        return segments.filter(this.#isString)
    }

    /** @param {string | undefined | null} value @returns {value is string} */
    static #isString(value) {
        return typeof value === BrowserPath.#stringType
    }

    /** @param {string[]} segments @returns {string[]} */
    static #withoutEmpty(segments) {
        return segments.filter(this.#hasText)
    }

    /** @param {string} segment @returns {boolean} */
    static #hasText(segment) {
        return segment.length > BrowserPath.#emptyLength
    }

    /** @param {string} joined @returns {string} */
    static #relativeOrDot(joined) {
        if (joined.length === this.#emptyLength) return this.#currentDirectory
        return joined
    }

    /** @param {string} joined @param {string} relative @param {boolean} absolute @returns {string} */
    static #rootedPath(joined, relative, absolute) {
        if (absolute) return this.#separator + joined
        return relative
    }

    /** @param {number} index @returns {number} */
    static #parentEnd(index) {
        if (index === this.#firstIndex) return 1
        return index
    }

    /** @param {string} parent @param {number} index @returns {string} */
    static #parentOrDot(parent, index) {
        if (index < this.#firstIndex) return this.#currentDirectory
        return parent
    }

    /** @param {string} from @param {string} value */
    static #resolutionTarget(from, value) {
        if (typeof value !== this.#stringType) return { value: from, standalone: true }
        return { value, standalone: value.startsWith(this.#separator) }
    }

    /** @param {string} from @param {{value: string, standalone: boolean}} target @returns {string} */
    static #resolutionInput(from, target) {
        if (target.standalone) return target.value
        return `${from}${this.#separator}${target.value}`
    }

    /** @param {string} path @returns {string[]} */
    static #pathSegments(path) {
        return this.#withoutEmpty(this.normalize(path).split(this.#separator))
    }

    /** @param {string[]} from @param {string[]} to @returns {number} */
    static #commonLength(from, to) {
        let common = 0
        while (this.#sameSegment(from, to, common)) common++
        return common
    }

    /** @param {string[]} from @param {string[]} to @param {number} index @returns {boolean} */
    static #sameSegment(from, to, index) {
        return index < Math.min(from.length, to.length) && from[index] === to[index]
    }

    /** @param {string} parent @param {string} normalized @param {string} target */
    static #checkDifferentPaths(parent, normalized, target) {
        if (parent === normalized) return
        this.#checkOffset(this.relative(parent, normalized), target)
    }

    /** @param {string} offset @param {string} target */
    static #checkOffset(offset, target) {
        this.#rejectParentPath(offset, target)
        this.#rejectAbsolutePath(offset, target)
    }

    /** @param {string} offset @param {string} target */
    static #rejectParentPath(offset, target) {
        if (offset.startsWith(this.#parentDirectory)) {
            throw new Error(`${this.#unsafePathMessage}${target}`)
        }
    }

    /** @param {string} offset @param {string} target */
    static #rejectAbsolutePath(offset, target) {
        if (offset.startsWith(this.#separator)) {
            throw new Error(`${this.#unsafePathMessage}${target}`)
        }
    }
}
