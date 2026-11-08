import { expect, test } from 'bun:test'
import { BrowserPath } from '../../src/browser/core/browser-path.js'

import { PathSamples } from './helpers/path-samples.js'

test.each([
    [PathSamples.empty, PathSamples.current],
    [PathSamples.current, PathSamples.current],
    ['///', PathSamples.root],
    ['a//b/./c/', 'a/b/c'],
    ['/a/../../b', PathSamples.absoluteSecond],
    ['a/../../b', PathSamples.parentSecond],
    [PathSamples.grandparent, PathSamples.grandparent],
    ['/../..', PathSamples.root],
    ['a/..', PathSamples.current],
    ['a/../..', PathSamples.parent],
    [PathSamples.dottedName, PathSamples.dottedName],
    [PathSamples.backslashName, PathSamples.backslashName],
    ['/日本/./語', '/日本/語'],
    [null, PathSamples.empty],
    [123, PathSamples.empty]
])('normalizing %p preserves the result %p', (input, expected) => {
    expect(BrowserPath.normalize(input)).toBe(expected)
})

test('browser paths join optional and absolute segments', () => {
    expect(
        BrowserPath.join(
            undefined,
            null,
            PathSamples.empty,
            PathSamples.first,
            PathSamples.empty,
            PathSamples.second
        )
    ).toBe(PathSamples.child)
    expect(BrowserPath.join()).toBe(PathSamples.current)
    expect(BrowserPath.join(PathSamples.first, PathSamples.absoluteSecond)).toBe(PathSamples.child)
    expect(BrowserPath.join(PathSamples.root, PathSamples.first, PathSamples.parent)).toBe(
        PathSamples.root
    )
})

test.each([
    [PathSamples.root, PathSamples.root, PathSamples.empty],
    [PathSamples.first, PathSamples.current, PathSamples.first],
    [PathSamples.absoluteFirst, PathSamples.root, PathSamples.first],
    ['a/b/', PathSamples.first, PathSamples.second],
    [PathSamples.empty, PathSamples.current, PathSamples.current],
    [PathSamples.parentFirst, PathSamples.parent, PathSamples.first]
])('path %p has parent %p and basename %p', (input, parent, name) => {
    expect(BrowserPath.dirname(input)).toBe(parent)
    expect(BrowserPath.basename(input)).toBe(name)
})

test('browser paths preserve resolution', () => {
    expect(BrowserPath.resolve(PathSamples.absoluteChild, PathSamples.parentThird)).toBe(
        PathSamples.absoluteSibling
    )
    expect(BrowserPath.resolve(PathSamples.absoluteChild, PathSamples.absoluteThird)).toBe(
        PathSamples.absoluteThird
    )
    expect(BrowserPath.resolve(PathSamples.absoluteChild, null)).toBe(PathSamples.absoluteChild)
    expect(BrowserPath.resolve(PathSamples.absoluteChild, PathSamples.empty)).toBe(
        PathSamples.absoluteChild
    )
    expect(BrowserPath.resolve(PathSamples.first, PathSamples.parentSecond)).toBe(
        PathSamples.second
    )
})

test.each([
    [PathSamples.absoluteChild, PathSamples.absoluteChild, PathSamples.empty],
    [PathSamples.absoluteChild, PathSamples.absoluteSibling, PathSamples.parentThird],
    [PathSamples.absoluteFirst, PathSamples.absoluteDescendant, 'b/c'],
    [PathSamples.absoluteDescendant, PathSamples.absoluteFirst, PathSamples.grandparent],
    [PathSamples.root, PathSamples.absoluteFirst, PathSamples.first],
    [PathSamples.absoluteFirst, PathSamples.root, PathSamples.parent],
    [PathSamples.child, 'a/c', PathSamples.parentThird],
    [PathSamples.empty, PathSamples.first, PathSamples.parentFirst]
])('relative path from %p to %p remains %p', (from, to, expected) => {
    expect(BrowserPath.relative(from, to)).toBe(expected)
})

test.each([PathSamples.containedRoot, '/root/docs/a', '/root/docs/../a'])(
    'contained path %p is accepted',
    (target) => {
        expect(() => BrowserPath.assertPathInside(PathSamples.containedRoot, target)).not.toThrow()
    }
)

test.each(['/other', '/root/../escape', '/rooted', '/root/..file'])(
    'escaping path %p is rejected',
    (target) => {
        expect(() => BrowserPath.assertPathInside(PathSamples.containedRoot, target)).toThrow(
            `Unsafe document path: ${target}`
        )
    }
)

test('unsafe path diagnostics retain string conversion for invalid input objects', () => {
    const target = {
        toString: () => 'external',
        valueOf: () => 42
    }
    expect(() => BrowserPath.assertPathInside(PathSamples.containedRoot, target)).toThrow(
        'Unsafe document path: external'
    )
})
