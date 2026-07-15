/**
 * Matches SQL LIKE patterns in O(value.length + pattern.length) time and O(1)
 * space. `%` matches zero or more UTF-16 code units and `_` matches exactly
 * one. The greedy backtracking point is always the most recent `%`, so an
 * adversarial pattern cannot trigger regular-expression backtracking.
 *
 * @param {string} value
 * @param {string} pattern
 * @param {{ singleCharacterWildcard?: boolean }} [options]
 * @returns {boolean}
 */
export function matchesLike(value, pattern, options = {}) {
    const singleCharacterWildcard = options.singleCharacterWildcard ?? true
    let valueIndex = 0
    let patternIndex = 0
    let wildcardIndex = -1
    let wildcardValueIndex = 0

    while (valueIndex < value.length) {
        const token = pattern[patternIndex]
        if ((singleCharacterWildcard && token === '_') || token === value[valueIndex]) {
            valueIndex++
            patternIndex++
            continue
        }
        if (token === '%') {
            wildcardIndex = patternIndex++
            wildcardValueIndex = valueIndex
            continue
        }
        if (wildcardIndex !== -1) {
            patternIndex = wildcardIndex + 1
            valueIndex = ++wildcardValueIndex
            continue
        }
        return false
    }

    while (pattern[patternIndex] === '%') patternIndex++
    return patternIndex === pattern.length
}
