import { spawn } from 'node:child_process'

export function startInherited(command, args, options = {}) {
  return spawn(command, args, {
    stdio: 'inherit',
    env: process.env,
    ...options,
  })
}

export async function runInherited(command, args, options = {}) {
  const child = startInherited(command, args, options)
  const exitCode = await new Promise((resolve, reject) => {
    child.on('error', reject)
    child.on('exit', resolve)
  })

  if (exitCode !== 0) {
    process.exit(Number(exitCode) || 1)
  }
}
