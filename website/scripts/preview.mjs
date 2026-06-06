import { spawn } from 'node:child_process'

const processes = []

function start(command, args) {
  const child = spawn(command, args, {
    stdio: 'inherit',
    env: process.env,
  })

  processes.push(child)

  child.on('exit', (code) => {
    if (code && code !== 0) {
      shutdown()
      process.exit(code)
    }
  })

  return child
}

function shutdown(signal = 'SIGTERM') {
  for (const child of processes) {
    if (!child.killed) child.kill(signal)
  }
}

process.on('SIGINT', () => {
  shutdown('SIGINT')
  process.exit(0)
})

process.on('SIGTERM', () => {
  shutdown('SIGTERM')
  process.exit(0)
})

start('bun', ['./scripts/bundle.mjs', '--watch'])
start('bunx', ['tach.preview'])
