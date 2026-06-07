#!/usr/bin/env bun
// Root route — returns health check. Static pages are handled by the Tachyon shell.
export class Handler {
  static GET() {
    return { ok: true, framework: 'Tachyon' }
  }
}
