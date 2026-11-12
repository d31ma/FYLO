// The size gate for the phone libraries (ADR 0026, section 10): the stripped
// libfylo_ffi built with the `mobile` profile must stay within its budget in
// size-budget.json. Each budget is the first measurement (2026-11-11) plus
// 10%; raising one is a reviewed change to that file.
//
//   bun scripts/mobile/check-size.mjs
//
// iOS is an open item: its budget comes with the xcframework (slice S5).
import { readFileSync, statSync } from 'node:fs'
import { CargoArtifacts } from '../cargo-artifacts.mjs'

class SizeGate {
    static LIBRARY = 'libfylo_ffi.so'
    static PROFILE = 'mobile'

    static run() {
        const budgets = JSON.parse(
            readFileSync(new URL('./size-budget.json', import.meta.url), 'utf8')
        )
        const over = Object.entries(budgets).filter(([target, budget]) =>
            SizeGate.over(target, budget)
        )
        process.exit(over.length === 0 ? 0 : 1)
    }

    static over(target, budget) {
        const size = statSync(CargoArtifacts.path(target, SizeGate.PROFILE, SizeGate.LIBRARY)).size
        const verdict = size <= budget ? 'ok  ' : 'OVER'
        console.log(`${verdict} ${target} ${SizeGate.LIBRARY} ${size} of ${budget} bytes`)
        return size > budget
    }
}

SizeGate.run()
