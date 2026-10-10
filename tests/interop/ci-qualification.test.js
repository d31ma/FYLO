import { describe, expect, test } from 'bun:test'

describe('Rust CI qualification contract', () => {
    test('the main workflow is reusable and preserves FYLO-specific safety gates', async () => {
        const workflow = await Bun.file('.github/workflows/ci.yml').text()

        expect(workflow).toContain('workflow_call:')
        expect(workflow).toContain('cancel-in-progress: true')
        expect(workflow).toContain('portable-artifacts:')
        expect(workflow).toContain('native:')
        expect(workflow).toContain('browser:')
        expect(workflow).toContain('binary-interop:')
        expect(workflow).toContain('miri:')
        expect(workflow).toContain('rust:crash:matrix')
        expect(workflow).toContain('run-rust-soak.mjs --iterations 100')
        for (const browser of ['chromium', 'firefox']) {
            expect(workflow).toContain(
                `- browser: ${browser}\n                      os: ubuntu-24.04`
            )
        }
        expect(workflow).toContain('- browser: webkit\n                      os: macos-15')
        expect(workflow).toContain('windows-2022')
        expect(workflow).toContain('windows-2025')
    })

    test('quality uses the locked whole workspace and rejects documentation warnings', async () => {
        const [manifest, workflow] = await Promise.all([
            Bun.file('package.json').json(),
            Bun.file('.github/workflows/ci.yml').text()
        ])

        expect(manifest.scripts['rust:check']).toContain(
            'cargo check --workspace --all-targets --all-features --locked'
        )
        expect(manifest.scripts['rust:clippy']).toContain(
            'cargo clippy --workspace --all-targets --all-features --locked'
        )
        expect(manifest.scripts['rust:test']).toContain(
            'cargo test --workspace --all-targets --all-features --locked'
        )
        expect(manifest.scripts['rust:doc']).toContain(
            'cargo doc --workspace --all-features --no-deps --locked'
        )
        expect(workflow).toContain('bun run rust:check')
        expect(workflow).toContain('bun run rust:doc')
        expect(workflow).toContain('RUSTDOCFLAGS: -D warnings')
    })

    test('the pinned Rust launcher waits for captured paths and rejects empty output', async () => {
        const launcher = await Bun.file('scripts/run-rust.mjs').text()

        expect(launcher).toContain("child.once('close', (code) =>")
        expect(launcher).toContain('rustup which ${binary} returned an empty path')
    })

    test('supply-chain, coverage, and provenance tools are exact and retained', async () => {
        const [manifest, workflow] = await Promise.all([
            Bun.file('package.json').json(),
            Bun.file('.github/workflows/ci.yml').text()
        ])

        expect(workflow).toContain('supply-chain:')
        expect(workflow).toContain('cargo-deny --version 0.19.7 --locked')
        expect(workflow).toContain('coverage:')
        expect(workflow).toContain('cargo-llvm-cov --version 0.8.6 --locked')
        expect(workflow).toContain('target/coverage.lcov')
        expect(manifest.scripts['rust:mcdc']).toBe('bun ./scripts/verify-rust-mcdc.mjs')
        expect(workflow).toContain('bun run rust:mcdc')
        expect(workflow).toContain('name: rust-mcdc-${{ github.sha }}')
        expect(workflow).toContain('target/mcdc/report.json')
        expect(manifest.scripts['rust:coverage']).toContain('--fail-under-lines 50')
        expect(manifest.scripts['rust:coverage']).toContain('--fail-under-functions 43')
        expect(manifest.scripts['rust:coverage']).toContain('--fail-under-regions 48')
        expect(workflow).toContain('provenance:')
        expect(workflow).toContain('cargo-cyclonedx --version 0.5.7 --locked')
        expect(workflow).toContain('cargo-auditable --version 0.7.0 --locked')
        expect(workflow).toContain('cargo cyclonedx --all --format json')
        expect(workflow).toContain('cargo auditable build')
    })

    test('MC/DC qualification is source-linked, fail-closed, and explicitly scoped', async () => {
        const [manifest, verifier, guide] = await Promise.all([
            Bun.file('tests/mcdc/decisions.json').json(),
            Bun.file('scripts/verify-rust-mcdc.mjs').text(),
            Bun.file('docs/operations/mcdc-qualification.md').text()
        ])

        expect(manifest.criterion).toBe('unique-cause-mcdc')
        expect(manifest.scope).toContain('not whole-core')
        expect(manifest.decisions).toHaveLength(6)
        expect(
            manifest.decisions.reduce((sum, decision) => sum + decision.conditions.length, 0)
        ).toBe(27)
        expect(new Set(manifest.decisions.map(({ id }) => id)).size).toBe(6)
        expect(verifier).toContain('another condition changed in the independence pair')
        expect(verifier).toContain('compiled MC/DC proof tests')
        expect(verifier).toContain('emitted MC/DC proof files')
        expect(guide).toContain('Fylo does not yet match SQLite')
        expect(guide).toContain('100% branch and MC/DC coverage of its core')
    })

    test('scheduled qualification exercises memory and concurrency sanitizers', async () => {
        const workflow = await Bun.file('.github/workflows/rust-nightly.yml').text()

        expect(workflow).toContain('nightly-2026-07-20')
        expect(workflow).toContain('-Zsanitizer=address')
        expect(workflow).toContain('-Zsanitizer=leak')
        expect(workflow).toContain('-Zsanitizer=thread')
        expect(workflow).toContain('-p fylo-storage-native')
    })
})
