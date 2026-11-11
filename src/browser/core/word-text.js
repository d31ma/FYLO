/**
 * Turns text into the normalized words of a word index (ADR 0023 section 2).
 * `crates/fylo-query/src/word_text.rs` implements the same rules;
 * `tests/fixtures/word-normalization-v1.json` keeps the two equal.
 */
export class WordText {
    /** Longest word kept, in UTF-8 bytes. */
    static MAX_WORD_BYTES = 64
    /** Most terms one `$match` operand may hold. */
    static MAX_TERMS = 16
    /** Folded forms of U+00C0 to U+017F; digits stand for two letters. */
    static LATIN_FOLDS =
        'aaaaaa1ceeeeiiiidnooooo-ouuuuy23aaaaaa1ceeeeiiiidnooooo-ouuuuy2yaaaaaaccccccccddddeeeeeeeeeegggggggghhhhiiiiiiiiii44jjkkkllllllllllnnnnnnnnnoooooo55rrrrrrssssssssttttttuuuuuuuuuuuuwwyyyzzzzzzs'
    /** @type {Record<string, string>} */
    static EXPANSIONS = { 1: 'ae', 2: 'th', 3: 'ss', 4: 'ij', 5: 'oe' }
    static FIRST_FOLDED = 0xc0
    static FIRST_COMBINING_MARK = 0x300
    static LAST_COMBINING_MARK = 0x36f
    static FINAL_SIGMA = 'ς'
    static SIGMA = 'σ'
    static WORD_CHARACTER = /^[\p{Alphabetic}\p{N}]$/u

    /**
     * The distinct words of `text`, in the order they first appear.
     * @param {string} text
     * @returns {string[]}
     */
    static words(text) {
        /** @type {string[]} */
        const words = []
        const current = { text: '', bytes: 0 }
        for (const character of text) {
            WordText.readCharacter(character, current, words)
        }
        WordText.finishWord(current, words)
        return words
    }

    /**
     * Whether every term is the start of at least one word of `texts`.
     * @param {string[]} texts
     * @param {string[]} terms
     * @returns {boolean}
     */
    static matchesTerms(texts, terms) {
        const words = texts.flatMap((text) => WordText.words(text))
        return terms.every((term) => WordText.hasPrefix(words, term))
    }

    /**
     * @param {string[]} words
     * @param {string} term
     * @returns {boolean}
     */
    static hasPrefix(words, term) {
        return words.some((word) => word.startsWith(term))
    }

    /**
     * @param {string} character
     * @param {{ text: string, bytes: number }} current
     * @param {string[]} words
     * @returns {void}
     */
    static readCharacter(character, current, words) {
        if (WordText.isCombiningMark(character)) return
        if (!WordText.WORD_CHARACTER.test(character)) {
            WordText.finishWord(current, words)
            return
        }
        for (const lower of character.toLowerCase()) {
            WordText.pushFolded(lower, current)
        }
    }

    /**
     * @param {{ text: string, bytes: number }} current
     * @param {string[]} words
     * @returns {void}
     */
    static finishWord(current, words) {
        const word = current.text
        current.text = ''
        current.bytes = 0
        if (word === '' || words.includes(word)) return
        words.push(word)
    }

    /**
     * @param {string} character
     * @param {{ text: string, bytes: number }} current
     * @returns {void}
     */
    static pushFolded(character, current) {
        if (WordText.isCombiningMark(character)) return
        const folded = WordText.fold(character)
        const bytes = WordText.utf8Length(folded)
        if (current.bytes + bytes > WordText.MAX_WORD_BYTES) return
        current.text += folded
        current.bytes += bytes
    }

    /**
     * @param {string} character
     * @returns {string}
     */
    static fold(character) {
        if (character === WordText.FINAL_SIGMA) return WordText.SIGMA
        const folded = WordText.LATIN_FOLDS[WordText.codePoint(character) - WordText.FIRST_FOLDED]
        if (folded === undefined) return character
        return WordText.EXPANSIONS[folded] ?? folded
    }

    /**
     * @param {string} character
     * @returns {boolean}
     */
    static isCombiningMark(character) {
        const point = WordText.codePoint(character)
        return point >= WordText.FIRST_COMBINING_MARK && point <= WordText.LAST_COMBINING_MARK
    }

    /**
     * @param {string} character
     * @returns {number}
     */
    static codePoint(character) {
        return character.codePointAt(0) ?? 0
    }

    /**
     * @param {string} text
     * @returns {number}
     */
    static utf8Length(text) {
        let bytes = 0
        for (const character of text) {
            bytes += WordText.characterBytes(WordText.codePoint(character))
        }
        return bytes
    }

    /**
     * @param {number} point
     * @returns {number}
     */
    static characterBytes(point) {
        if (point < 0x80) return 1
        if (point < 0x800) return 2
        if (point < 0x10000) return 3
        return 4
    }
}
