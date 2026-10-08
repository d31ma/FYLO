# ADR 0011: Bounded serverless invocation

Status: accepted for isolated source implementation. Native behavior validation
and runtime/backend qualification remain pending. No deployment is authorized.

## Context

Serverless lifetime is separate from metadata representation. FYLO owns the
request, storage, recovery and teardown contracts; PaaS owns AWS infrastructure.
`efs-sidecar-v1` is one unqualified backend candidate, not a general runtime mode.
Object stores do not inherit POSIX rename, sync or advisory-lock guarantees.

The existing streaming machine serves frames until EOF and flushes responses
while its session holds leases. `persistent=false` disables cursors; it does not
establish invocation finalization. Legacy streaming and native opens retain
their behavior.

## Decision

Add `InvocationOptions` and `InvocationRuntime` to the machine crate:

```rust
InvocationOptions::new(root, application, limits, (deadline, cleanup_reserve))
    -> std::io::Result<InvocationOptions>;
InvocationRuntime::new(options) -> std::io::Result<InvocationRuntime>;
InvocationRuntime::execute(&self, frame: &[u8]) -> std::io::Result<Vec<u8>>;
```

Trusted host configuration selects the root, repository, metadata profile,
application configuration and published frame limits. Externally leased options
are refused because their caller may retain a clone. The selected repository
must already exist and be the same actual directory; canonical aliases are
allowed. Request `root` may
only name that same root, and request `schemaDir` cannot override host policy.
Existing operation data and application authority retain their contracts.

This first invocation slice refuses `log`, `commit`, `checkout`, `branch`,
`status`, `diff`, `restoreCommit` and `merge` before lease acquisition. Their
additional repository read sets are not qualified. The reused core handshake
keeps its legacy capabilities; it does not advertise support for those operations
through this invocation API. Malformed input and forbidden host overrides return
an admission error rather than a machine failure frame.

One invocation accepts exactly one nonempty LF-delimited core request. Reject
empty, truncated, multiple or oversized frames before admission or mutation.
The host supplies an absolute monotonic deadline and a nonzero cleanup reserve.
Reuse within the same budget is permitted; a host assigns a fresh budget for
each subsequent provider invocation rather than retaining an expired deadline.

The runtime acquires its actual-root lease internally and requires supported
kernel advisory admission. Recover repository materialization, then live and
catalogued collections in the main root and active worktree before dispatch.
Enumeration rejects unsafe directory/entry types and is capped at 10,000 unique
collection names per root. Construct fresh private engines and keep cursors
disabled. Check the deadline/reserve at admission, during enumeration/recovery and
after recovery before dispatch. No mutable engine, cursor or owned handle is
returned or retained between calls. Warm derived caches must revalidate changed
storage generations through the existing engine contract. The authored warm
result-revalidation test does not exercise a field equality-index cache hit;
that regression remains a qualification gap.

Buffer the existing machine-v1 response privately. Drop private session and
engine handles, then consume the retained lease through `release_checked`.
Use atomic unique ownership transfer with `Arc::try_unwrap`; observing a strong
count alone does not establish uniqueness. Remove only the currently verified
owner metadata while holding the advisory lock, sync through the existing
filesystem primitive, and check unlock. Preserve the kernel lock inode.
Legacy `Drop` retains its existing diagnostic metadata and unlock behavior.

Return response bytes only after checked finalization. A domain failure may
return a bounded machine failure frame after successful teardown. Ownership,
cleanup or unlock uncertainty returns an error without completed response bytes.
Always attempt checked teardown after an admitted request, including execution
or response-encoding errors. No automatic mutation replay is added.

## Limits

Deadline checks are cooperative. They do not preempt blocking filesystem I/O,
external validators or an operation already committing. A lost response, timeout
or interrupted mutation can have an uncertain outcome; callers must not assume
rollback or retry an arbitrary mutation. Worker termination and response-freeze
behavior require a qualified host supervisor.

The private output slice and returned frame are bounded. Reused serializers may
still create larger temporary JSON values or strings; this contract does not
provide a hard aggregate memory bound.

Advisory locks and owner metadata provide cooperative admission, not distributed
fencing. The inherited directory-sync primitive can tolerate `Unsupported`;
remote durability remains unqualified. General durable idempotency needs request
equivalence and an outcome committed atomically with the mutation. This slice
adds no receipt format, lease TTL, fencing token or exactly-once promise.

The EFS sidecar identity-binding gap, EFS queue constructor seam and independent
client/cache/lock-loss qualification remain separate gates. This API does not
promote EFS, Lambda or another provider to supported production operation.

## Evidence and qualification

Preserve the prior 26-file source candidate `8edb82c` and invocation tests-only
checkpoint `8938dce` before implementation. The 22 authored public-API contracts
cover frames, budget, trusted configuration, root aliases/contention, checked
teardown, cold recovery, warm cache inputs, bounded errors, no automatic replay
and legacy streaming/Drop behavior. They have not been executed under the
source-only grant; syntax checks are not actual RED/GREEN evidence.

The next gate is bounded native storage, engine, machine and invocation tests,
followed by a newly built exact candidate binary, interop and local crash gates.
Keep initial failures, fix within assigned ownership and re-pin the source.
Live EFS and provider lifecycle qualification require separate concrete scope.

See [ADR 0010](0010-explicit-efs-metadata-profile.md), the
[machine contract](../../api/machine/v1/README.md) and the
[backend qualification plan](../operations/efs-profile-qualification.md).
