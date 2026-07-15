// Single source of truth for the client-count copy across the site.
//
// Two kinds of clients:
//   - Thin shims: spawn the `fylo` binary and speak the machine protocol over
//     stdin/stdout. One drop-in file per language (clients/<lang>/).
//   - Local-only clients: embed FYLO's engine on-device (OPFS/WebView) — each
//     device owns its store. The browser bundle and the iOS/Android clients.
export const SHIM_LANGUAGES = [
  'Python',
  'Ruby',
  'Node',
  'PHP',
  'Go',
  'Rust',
  'C#',
  'Java',
  'Dart',
]
export const SHIM_COUNT = SHIM_LANGUAGES.length

// Local-only clients (embed the engine on-device), not machine-interface shims.
export const LOCAL_FIRST_CLIENTS = [
  'Browser (JS)',
  'iOS (Swift)',
  'Android (Kotlin)',
  'Flutter (Dart)',
]

// Every language client shown on the site (shims + local-first). Matches the
// number of language tabs; use this for "N languages/clients" copy.
export const CLIENT_COUNT = SHIM_COUNT + LOCAL_FIRST_CLIENTS.length
