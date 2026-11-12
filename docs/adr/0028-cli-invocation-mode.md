# ADR 0028: CLI invocation mode (`fylo exec --invocation`)

- Status: **Accepted** for local use (accepted by the user, 2026-11-12). Not
  qualified for deployment: ADR 0011's "No deployment is authorized" still
  holds for this mode.
- Date: **2026-11-12**
- Owners: **FYLO maintainers**
- Extends: ADR 0011 (the `InvocationRuntime` it defines is now reachable
  from the `fylo` binary) and amends its teardown order (logs are
  checkpointed before the lease is released).

## Context

Serverless hosts (TACHYON's Lambda containers) run one `fylo` process per
request against one shared root. Every `fylo exec` session takes the root's
kernel lease on its first request and does not wait for it ("Root owner" in
`README.md`), so a request that meets another process's lease fails at
once: on v26.45.07 a one-shot request on a root an `exec --loop
--exclusive-root` held answered `EROOTLOCKED` in about 6 ms. ADR 0011's
`InvocationRuntime` already waits, with bounded backoff, while another
process holds the root, but only Rust hosts linking `fylo-machine` could
use it.

## Decision

### 1. The command

```
fylo exec --invocation --request <json|@path|-> \
  --deadline-ms <n> --cleanup-reserve-ms <n> [--root <path>] \
  [--filesystem-profile <native|efs-sidecar-v1>] \
  [--max-request-bytes <n>] [--max-response-bytes <n>] [--config <path>]
```

One process answers one request through `InvocationRuntime::execute`, then
exits. The deadline is counted in milliseconds from process start. The
cleanup reserve is the part of it kept for release; it must be nonzero and
smaller than the deadline. The root is `--root`, else `fylo.root`, and must
already exist. `--loop` and `--exclusive-root` are refused with it.

Every ADR 0011 rule applies unchanged: the root lease is taken for this one
request and waited for with backoff up to 200 ms until the budget reaches
its reserve; `putData` needs a caller-chosen `id`; cursors are off; the
repository operations are refused; the request frame limit is the same
1 MiB default; a request may not override the root or schema directory.

### 2. Output, exit codes and error codes

The same as `fylo exec --request`, so one driver reads both:

- An answer frame, one JSON line on stdout. Exit 0 when it is `ok`, else 1.
- A host configuration error (a missing or invalid flag, request JSON that
  is not an object, no root) goes to stderr with exit 1 and no frame.
- A refusal by the runtime is a failure frame with the same envelope
  (`protocolVersion`, `ok`, `op`, `requestId`, `durationMs`,
  `error.name`, `error.message`, `error.code`) and these codes:

| Code | When |
| --- | --- |
| `EROOTLOCKED` | Another process held the root until the budget reached the reserve; nothing was changed |
| `EDEADLINE` | The deadline is at or below the reserve, or ran out during recovery or dispatch |
| `EBADREQUEST` | `putData` without `id`, a zero reserve, a root or `schemaDir` override |
| `EFRAME_REQUEST_TOO_LARGE` | The compact request is larger than `--max-request-bytes` |
| `EUNSUPPORTEDOP` | `log`, `commit`, `checkout`, `branch`, `status`, `diff`, `restoreCommit`, `merge` |
| `EQUERYLOOPREQUIRED` | A paged query (`page`), as for `exec --request` |
| `EINVOCATION` | A teardown or unlock failure with no stable code; the outcome is uncertain |

Any other code is the operation's own (`ENATIVE_*`, `EINDEX_FULL`, ...).
A failure after admission can follow a committed mutation (ADR 0011,
"Limits"): retry only `putData` with the same `id` and idempotent updates.

### 3. Teardown order

`InvocationRuntime::execute` now checkpoints the write-ahead logs (ADR 0013)
this process wrote under the root before it releases the lease. A checkpoint
takes the collection lock; taken after the release, it met the next owner's
first write, which then failed with `ENATIVE_CONCURRENT_WRITE`. A checkpoint
failure is returned as an error after the release, like other cleanup
uncertainty. The CLI mode does no further log close after the runtime.

## Consequences

- Two processes on one root serialize instead of failing: the tests run two
  workers of 15 one-shot invocation processes each on one root with no
  `EROOTLOCKED`.
- Each invocation pays a process start, recovery and a checkpoint. On the
  Mac mini (APFS, local) one invocation process took p50 10.3 ms / p95
  11.6 ms for `getDoc` and p50 15.3 ms / p95 16.4 ms for `putData`
  (`invocation_cost`, ignored test in `crates/fylo-cli/tests`).
- The lease is cooperative, kernel-admitted and local. Network filesystems
  (EFS included), provider lifecycle, response loss and fencing keep ADR
  0011's open qualification gates. This mode is a local and test tool until
  they pass.

## Evidence

`crates/fylo-cli/tests/invocation_command.rs` drives the real binary:
parallel invocation processes on one root, a root held by `exec --loop
--exclusive-root` (waits to the reserve, then `EROOTLOCKED` with the same
envelope and code as `exec --request` and no change; a release inside the
deadline lets the waiting invocation succeed), a deadline at or below the
reserve, `putData` ids and repeats, refused cursors and repository
operations, the frame limit and stderr host errors. ADR 0011's contract
tests (`serverless_invocation_contract`) still pass.
