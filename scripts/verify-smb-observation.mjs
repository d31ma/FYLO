import { SmbObservation } from './smb-observation.mjs'

await SmbObservation.cli(process.argv.slice(2)).catch((error) => {
    console.error(error)
    process.exitCode = 1
})
