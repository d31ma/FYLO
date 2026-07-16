#!/usr/bin/env bun

import path from 'node:path'
import { FyloS3Restore } from '../src/replication/s3-restore.js'

function usage() {
    return [
        'Usage:',
        '  bun scripts/s3-restore.mjs verify  --bucket <name> --prefix <key-prefix>',
        '  bun scripts/s3-restore.mjs restore --bucket <name> --prefix <key-prefix> --destination <new-path>',
        '',
        'Optional: --region, --endpoint, --concurrency, --max-object-bytes',
        'Credentials come from the standard AWS_* or FYLO_S3_* environment variables.'
    ].join('\n')
}

function parse(argv) {
    const [command, ...rest] = argv
    if (!['verify', 'restore'].includes(command)) throw new Error(usage())
    const values = {}
    for (let index = 0; index < rest.length; index += 2) {
        const flag = rest[index]
        const value = rest[index + 1]
        if (!flag?.startsWith('--') || value === undefined)
            throw new Error(`Invalid option: ${flag ?? ''}\n${usage()}`)
        values[flag.slice(2)] = value
    }
    if (!values.bucket || !values.prefix)
        throw new Error(`--bucket and --prefix are required\n${usage()}`)
    if (command === 'restore' && !values.destination)
        throw new Error(`--destination is required for restore\n${usage()}`)
    const number = (name) => {
        if (values[name] === undefined) return undefined
        const parsed = Number(values[name])
        if (!Number.isSafeInteger(parsed) || parsed < 1)
            throw new Error(`--${name} must be a positive integer`)
        return parsed
    }
    return {
        command,
        destination: path.resolve(
            values.destination ?? path.join(process.cwd(), '.fylo-verify-unused')
        ),
        s3: {
            bucket: values.bucket,
            prefix: values.prefix,
            region: values.region,
            endpoint: values.endpoint
        },
        options: {
            concurrency: number('concurrency'),
            maxObjectBytes: number('max-object-bytes'),
            onStatus(status) {
                process.stderr.write(`${JSON.stringify(status)}\n`)
            }
        }
    }
}

try {
    const args = parse(process.argv.slice(2))
    const recovery = new FyloS3Restore(args.s3, args.destination)
    const result = await recovery[args.command](args.options)
    process.stdout.write(`${JSON.stringify(result)}\n`)
} catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`)
    process.exitCode = 1
}
