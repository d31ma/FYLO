# Bounded SMB observation

This is a synthetic qualification harness for the remaining hard-link fallback
comparison. It does not change FYLO storage behavior or qualify general SMB
support. The existing serverless lane remains unchanged.

One invocation observes one backing filesystem. Run FAT and tmpfs serially,
under a separately admitted shared heavy slot, with identical source, binary,
image and common configuration pins. Record the filesystem difference as a
confounder: it changes more than hard-link support.

## Execution gates

No WSL/SMB run is currently admitted. Project I/O requires explicit clearance
from recovery owner `01a12c31-5976-747d-845c-00610048b0cc`, fresh parent resource
admission, source/dirty-state reconciliation and exclusive resource ownership.
The runner does not recover WSL, install dependencies, build binaries, pull
images or waive the merge hold.

Prepare a reviewed admission plan with locally present immutable image IDs,
prebuilt Linux binaries and their provenance/digests, exact source/tool pins,
absolute synthetic output paths and expiring owner/health/resource receipts.
These inputs record admission; supplying a JSON file does not grant authority.
Keep the same candidate and pins for both lanes.

The new bounded fixture uses a 96-MiB FAT image inside a 128-MiB tmpfs, or a
128-MiB tmpfs share. This differs from the historical 3-GiB FAT fixture. Treat
results as a new synthetic observation rather than an exact reproduction.
Both lanes preserve SMB byte-range locks and root fencing; `nobrl` is prohibited.

## Evidence and interpretation

The retained bundle must include the admitted plan, source/binary/image/tool
identities, unique namespace and owned Docker IDs, resource limits, mount and
backing configuration, link result/errno and fencing results.

Request-correlated events retain complete raw stdout, stderr, exit status,
HTTP status/body and timestamps for successful and failed responses. The
12-request A burst, stop request/completion and final A exit/signal/OOM state
are separate events. An ACK observed during the grace period must not be
described as having arrived before the stop request.

Persist exact ACK IDs and B's actual post-stop `findDocs` response before
assessment or the subsequent fresh write/read. Preserve successful evidence
as well as failures. Export client/server logs and synthetic fixture/lock
diagnostics before cleanup. A failed export or uncertain daemon operation
must retain the fixture and prohibit destructive cleanup.

| B's read                                                    | Result                                                                                |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| Failed, absent, malformed, rejected or transport-incomplete | Unknown; missing IDs cannot be inferred                                               |
| Valid successful listing and zero ACKs                      | Inconclusive                                                                          |
| Valid successful listing contains all exact own ACK keys    | Present in this read; payload correctness is assessed separately                      |
| Valid successful listing omits ACK keys                     | Missing from this read; reconcile fixture/generation evidence before attributing loss |

The old 10/12 ACK trace cannot establish acknowledged-write loss because B's
post-SIGTERM read was not retained. Its later `ENATIVE_UNSAFE_PATH` was a write
outcome. B's fresh write/read also cannot substitute for the missing observation.

Stopping A while Samba remains running tests process termination, not storage
server or host failure. Qualification and the original develop merge are
separate milestones; merge requires explicit release of its hold.

## Deferred local verification

No executable checks were run while implementing this harness. Use the exact
commands recorded in the task's verification plan only in the next admitted
local runtime window. Pure unit tests must not spawn Docker, launch services,
or access WSL/SMB. Keep the previous native/Wasm receipts; this harness-only
change does not require repeating those suites unless new evidence warrants it.

Run these commands sequentially only after admission to the next local runtime
window. They use the already available Node and cached exact Prettier 3.8.3;
there is no dependency-install step. Formatting changes only the new harness,
its tests and this guide. Refresh the new-source SHA256 pins after formatting.

```sh
cd /Users/iyor/Documents/Codex/2026-10-11/task-10/harness-worktree

/opt/homebrew/bin/node /Users/iyor/.bun/install/cache/prettier@3.8.3@@@1/bin/prettier.cjs --write scripts/smb-observation-config.mjs scripts/smb-observation-evidence.mjs scripts/smb-observation-journal.mjs scripts/smb-observation-process.mjs scripts/smb-observation.mjs scripts/verify-smb-observation.mjs tests/scripts/smb-observation.test.js docs/operations/smb-observation.md

/opt/homebrew/bin/node /Users/iyor/.bun/install/cache/prettier@3.8.3@@@1/bin/prettier.cjs --check scripts/smb-observation-config.mjs scripts/smb-observation-evidence.mjs scripts/smb-observation-journal.mjs scripts/smb-observation-process.mjs scripts/smb-observation.mjs scripts/verify-smb-observation.mjs tests/scripts/smb-observation.test.js docs/operations/smb-observation.md

/opt/homebrew/bin/node --check scripts/smb-observation-config.mjs
/opt/homebrew/bin/node --check scripts/smb-observation-evidence.mjs
/opt/homebrew/bin/node --check scripts/smb-observation-journal.mjs
/opt/homebrew/bin/node --check scripts/smb-observation-process.mjs
/opt/homebrew/bin/node --check scripts/smb-observation.mjs
/opt/homebrew/bin/node --check scripts/verify-smb-observation.mjs
/opt/homebrew/bin/node --check tests/scripts/smb-observation.test.js

/opt/homebrew/bin/node --test --test-concurrency=1 tests/scripts/serverless-outcome-assessment.test.js tests/scripts/smb-observation.test.js
git diff --check
```

## Separately admitted remote invocation

The entry point accepts `--plan /absolute/admission-plan.json`, `--backing fat`
or `--backing tmpfs`, and optional `--retain-fixture`. Importing the helper
modules must not launch Docker. The remote invocation is distinct from the
local verification above and remains held until health/resource admission.

Use the pinned Node executable from the plan. An operator prepares and reviews
the plan after clearance; the following is a command shape, not a current
execution instruction:

```sh
/absolute/pinned/node scripts/verify-smb-observation.mjs --plan /absolute/admission-plan.json --backing fat --retain-fixture
```

The plan requires `owner`, `sourceRoot`, `outputParent`, `hostMachineId`,
`dockerSocket`, `dockerDaemonId`, `dockerRootPath`, `hostCgroup`, `admission`,
`artifacts`, `tools` and `diskFloors`. The admitted Docker endpoint is a local
Unix socket. Host and daemon IDs must match the admission receipt.

Artifact pins cover both locally present
client/Azurite image IDs, three prebuilt binaries (`fylo`, `preview`, `http`),
the baseline candidate source and every new executed harness module. Require
`artifacts.sourceHead`, a digest-pinned `artifacts.provenance` file and the
per-file `artifacts.sources` map; these bind the Linux binaries to their
candidate source. Tool pins cover Node, Bun, Functions Core Tools, Docker
server, containerd and Python. Disk receipts
must cover the actual Docker and output backing volumes, rather than merely
the directory names. The generated UUID namespace is not supplied by callers.

`admission` requires `approvedAt`, `expiresAt`, `projectIo: true`,
`healthReady: true`, `exclusiveSlot: true`, the recovery owner's `healthOwner`,
matching `hostMachineId` and `dockerDaemonId`, finite `availableMiB` and
`backingDisks` receipts. Admission must be no older than five minutes at startup
and remain valid through the configured total deadline. Its lifetime cannot
exceed fifteen minutes. Each backing disk receipt contains `volume`,
`freeMiB` and `floorMiB`; `diskFloors` contains actual Linux `path`/`floorMiB`
entries for the Docker and evidence backing paths.

The runner must already be inside the admitted cgroup v2 scope. That scope
must enforce its host memory ceiling, zero swap, at most 256 processes and
one CPU for the driver and its descendants. The runner does not create or
reconfigure the scope. Default reservations total 3,584 MiB, including
1,024 MiB for the driver and its concurrent Docker CLI children; admission
requires another 2,048 MiB of headroom. Each Functions host is capped at
1,024 MiB, Samba and Azurite at 256 MiB each, and each container's writable
layer at 128 MiB. The evidence ceiling is 100 MiB.

Default deadlines are 300 seconds for observation and 420 seconds overall,
with at most 30 seconds per subprocess, 25 seconds per HTTP request and a
10-second SIGTERM grace period. The separate export reserve does not permit
new workload after the observation deadline.

The provenance JSON records the exact `sourceHead`, `sources` SHA256 values
for both native write files, and `binaries` SHA256 values keyed by `fylo`,
`preview` and `http`. Its digest is pinned in the admission plan. This is
retained build provenance supplied by the admitted preparation owner, rather
than a build performed by the observation runner.

Elapsed observation, readiness, subprocess and export budgets use monotonic
clocks. UTC timestamps remain in the evidence for correlation. The subprocess
budget reserves two seconds for terminating only its owned local CLI child:
SIGTERM, then SIGKILL after one second, then an exit-verification deadline.
This never establishes cancellation of the Docker daemon operation. Unknown
termination retains resources and requires the admission/recovery owner.

There are 31 new source-only test cases. The combined local command is expected
to exercise these plus the existing 30 assessment cases; no result is claimed
until the admitted check actually runs. Formatting and syntax are also pending.

The runner requires Docker writable-layer quotas. Unsupported storage quotas
are a prerequisite failure: do not change the host filesystem, weaken the
ceilings or substitute a broad legacy lane to get past it. A retained fixture
continues to consume its reserved resources; transfer its exact owned IDs and
evidence path to the admission owner before releasing that reservation.
