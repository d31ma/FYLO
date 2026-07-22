# Windows storage release gate

FYLO treats native Windows storage validation as a release requirement. The `windows-storage`
job in both CI and Release runs on GitHub-hosted x64 Windows Server 2022 and Windows Server 2025
runners. Both matrix entries must pass before a release can publish artifacts.

The gate uses the Bun version in `.bun-version` and fails immediately unless Bun reports
`win32/x64`. The Windows vendor installer resolves each dependency's latest published release,
downloads the executable and `SHA256SUMS` from that tagged release, and fails closed unless the
published checksum matches. It verifies:

- process-owned file-lock exclusion, crash release, and abandoned-lock reclamation;
- rollback and recovery after a transaction process is forcibly terminated;
- NTFS junction and reparse-point rejection at document and recovery boundaries;
- Windows metadata, durability, filesystem, collection-lock, raw-file, and version-recovery
  regressions;
- a freshly compiled `fylo.exe` persisting and reading a document through the public machine
  protocol.

Do not skip or replace this job with a Linux result. A Windows artifact is releasable only after
both native Windows matrix entries succeed.

## Local execution from Apple Silicon macOS

The development host currently has `act` 0.2.87 and Docker Desktop. `act --list` can parse and
list the Windows job, but it cannot execute it faithfully: Docker Desktop exposes a Linux/aarch64
engine, and `act` runs Actions jobs in Linux containers. Choosing or mapping an image for
`windows-2022` or `windows-2025` would therefore exercise Linux kernel and filesystem behavior,
not Windows process locks, NTFS, reparse points, or a native Windows executable.

Use `act` locally only for workflow discovery or Linux-job approximation:

```sh
act pull_request -W .github/workflows/ci.yml --list
```

Do not map a Windows label to an Ubuntu image and treat the result as Windows validation. Run the
authoritative job by pushing a branch or pull request to GitHub. For interactive failures, use an
x64 Windows Server VM with the repository checkout on an NTFS volume, then run the same commands
shown in the `windows-storage` job.
