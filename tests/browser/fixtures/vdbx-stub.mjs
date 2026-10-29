// A deterministic stand-in for a `.vdbx` file until the format exists.
//
// The bytes come from a seeded xorshift, so the page and the spec can both
// produce the file and agree on every byte without shipping a binary fixture.
// The first eight bytes are the `.vdbx` magic, the way a real file starts.

export const STUB_MAGIC = [0x89, 0x56, 0x44, 0x42, 0x58, 0x0d, 0x0a, 0x1a]

/** Odd on purpose: no read lines up with a power of two at the end. */
export const STUB_SIZE = 3 * 1024 * 1024 + 4099

export function stubBytes(size = STUB_SIZE, seed = 0x5eed) {
    const bytes = new Uint8Array(size)
    let state = seed >>> 0 || 1
    for (let index = 0; index < size; index++) {
        state ^= state << 13
        state ^= state >>> 17
        state ^= state << 5
        bytes[index] = state & 0xff
    }
    bytes.set(STUB_MAGIC.slice(0, Math.min(size, STUB_MAGIC.length)))
    return bytes
}

/** The same xorshift, for choosing offsets reproducibly. */
export function seededRandom(seed) {
    let state = seed >>> 0 || 1
    return () => {
        state ^= state << 13
        state ^= state >>> 17
        state ^= state << 5
        return (state >>> 0) / 0x100000000
    }
}
