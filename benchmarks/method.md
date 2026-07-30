# Evering IPC study method

This document fixes the experiment before substantive performance evidence is
collected. It is a validity contract, not a claim that Evering is universally
faster or slower than another transport.

## Question and claim boundary

The core study asks:

> Under which declared payload, capacity, in-flight, waiting, memory, and
> platform conditions does an Evering two-process request/response channel
> change throughput or recovery behavior relative to a matched blocking OS
> stream?

Latency and CPU cost are reportable only when the same admitted instrumentation
observes both arms in a separate mode. Queue, allocation, notification, and
process-exchange measurements can bound an explanation of a whole-system
difference; they do not prove its cause.

The core study does not compare threads with processes, pool platforms, treat a
runtime as a transport, silently substitute unsupported behavior, or add an
optional comparator before the matched experiment is complete.

## Logical operation

One accepted operation is exactly one request with:

- a monotonically assigned operation number;
- the declared payload length;
- deterministic payload bytes derived from the study seed and operation
  number.

One completed operation is exactly one response carrying the same operation
number and the declared deterministic transform of the complete request.
Validation checks every response byte.

A successful trial satisfies:

`requested = accepted = completed = validated`

A mismatch is a phase-specific failure with no performance value. Submitted
work is never treated as completed work.

## Contrast, arm, and block

A `ContrastKey` identifies semantic workload only:

- platform and target;
- payload;
- application-visible capacity;
- maximum in-flight operations;
- topology;
- shared extent and allocator geometry where applicable.

A `Contrast` combines that key with exactly two named arms:

- candidate: Evering with one of `busy`, `adaptive`, or `notified`;
- baseline: framed IPv4 loopback with policy `blocking`.

Implementation and arm policy are not fields of `ContrastKey`. The blocking
baseline is never duplicated under fake Evering policy labels.

One independent unit is one fresh coordinator/worker process trial for one arm.
Each block has exactly one trial for both arms of every registered contrast.
The seed deterministically randomizes contrast order, then arm order inside
each contrast. It never changes membership, work, or identity. A process is not
reused across independent units.

## Process and phase boundary

The core topology is one coordinator process, one worker process, and one
connection or typed request/response channel pair. Producer-count and
multi-worker claims require a separately registered experiment.

Every trial has:

1. setup: resource creation, mapping/admission, child spawn, handle exchange,
   runtime registration, and fixed-buffer allocation;
2. warmup/readiness: identical logical warmup followed by acknowledgement from
   both processes;
3. barrier: one release after both acknowledgements;
4. timed work: exact admitted requests, backpressure, allocation/copy required
   by the arm, response completion, and full validation;
5. drain: close, remaining notification consumption, child wait or exact
   kill-and-wait, reclamation, and unmapping;
6. evidence persistence outside the timed interval.

The clock starts at barrier release and stops after validation of the last
requested response. Setup, timed, and drain each have one absolute deadline.
An I/O retry cannot restart a phase timeout. Every spawned child reaches one
observed terminal state.

## Work and resource equivalence

- Both arms use the same exact requested count and deterministic payloads.
- Capacity is the maximum application-visible outstanding record count.
- Stream batching is `min(capacity, in_flight, remaining)`.
- The stream uses one connection and records actual socket-buffer sizes.
- Evering records actual shared extent and admitted allocator geometry.
- No equal-memory claim is made unless all relevant buffers and bounds were
  observed. Otherwise memory comparability is explicitly unavailable.
- Unsupported arm/condition pairs remain explicit without timing.
- Setup, warmup, drain, error, and validation rules are identical in meaning,
  even when their transport mechanics differ.

## Registered matrices

Reference values are payload 1024 bytes, capacity 8, in-flight 8, automatic
allocator geometry, and the actual resulting shared extent.

### Smoke

Smoke runs 100 operations as a correctness check only. It produces no
performance claim.

### Screening

Screening uses these 23 contrasts:

- all 15 combinations of payload `[0, 64, 1024, 16384, 65536]` and Evering
  policy `[busy, adaptive, notified]` at capacity 8 and in-flight 8;
- capacity `[1, 256]` at payload 1024, in-flight 8, notified;
- in-flight `[1, 64]` at payload 1024, capacity 8, notified;
- boundary pairs `(capacity, in-flight)` of `(1,1)`, `(1,64)`, `(256,1)`, and
  `(256,64)` at payload 1024, notified.

Screening uses 10 complete paired blocks per platform. Its intervals and
capacity/in-flight observations are descriptive.

### Focused confirmation

The focused family is fixed before screening: the 15 payload × Evering-policy
contrasts at capacity 8 and in-flight 8. It uses 30 complete paired blocks per
platform. Capacity, in-flight, memory-geometry, or topology confirmation
requires a later preregistered family.

Windows and openSUSE Tumbleweed are separate families and separate analyses.

## Pilot and stopping

Before screening, an excluded pilot runs both arms and selects one exact
operation count per contrast so the faster arm is expected to run for at least
250 ms without approaching the timed deadline. That count is identical for
both arms and frozen before block 0.

Pilot rows are not evidence. Trial counts, seeds, operation counts, warmup,
adaptive-spin bound, and deadlines are fixed before execution. There is no
data-dependent stopping, post-hoc outlier deletion, or conversion of screening
into confirmation.

## Waiting policies

- `busy` retries the same nonblocking shared operation without an OS wait.
- `adaptive` performs a recorded bounded spin, then follows the notified path.
- `notified` performs check-arm-recheck through the shipped sticky
  notification and immediately retries authoritative shared state after wake.
- `blocking` is the framed stream's kernel-mediated blocking behavior.

The three Evering policies share layouts, payload representation, operation
path, counts, validation, close, and cleanup. They differ only at retry.
Notification is advisory: it never publishes, consumes, closes, rolls back a
committed record, or proves peer death.

## Evidence format v2

One parser owns the tab-separated format. Fields cannot contain tabs, newlines,
or carriage returns.

`validate_prefix` admits metadata and each independently valid row from an
interrupted artifact. `validate_complete` additionally requires the footer,
the exact registered schedule, every arm pair and block, and terminal
consistency. Analysis accepts only `validate_complete`.

### Metadata row

The metadata records:

- format version and exact generating command;
- source revision, dirty-state flag, and dirty-diff digest;
- build profile, Rust compiler, target triple, OS/kernel build, and page size;
- CPU model/topology, declared affinity, and observable governor/power policy;
- dependency and admitted-comparator versions;
- wall-clock start, seed, mode, schedule identity, blocks, warmup, and
  operation-count policy;
- absolute setup, timed, and drain deadlines;
- adaptive-spin bound.

An unavailable environmental observation is encoded explicitly as unavailable,
not omitted or invented.

### Trial row

Every trial records:

- contrast, arm, block, and actual order;
- requested and actual payload, capacity, in-flight, batch, topology, policy,
  transport, shared extent, allocator geometry, and socket-buffer observations;
- phase durations;
- requested, accepted, completed, and validated counts;
- `success`, `unsupported`, or exact setup/timed/drain failure;
- elapsed nanoseconds only for valid success;
- optional symmetric process CPU/counter measurements;
- fallback, coalescing, retry, and health observations required to interpret
  the selected arm.

### Footer

The footer records schedule identity, expected and written row counts, and
terminal completion. A missing or mismatched footer prevents complete
validation.

### Persistence and exit

The runner creates a no-overwrite `.partial` artifact in the final directory.
It flushes and `sync_data`s metadata, then appends, flushes, and `sync_data`s
each independently valid trial. After all rows validate, it writes the footer,
flushes and `sync_all`s, atomically publishes the same inode with a
same-directory no-overwrite hard link, then removes the partial name.
Unsupported hard-link publication fails without copying or overwriting
evidence. Persistence is outside measured time.

An interrupted prefix remains readable but incomplete. Any mandatory scheduled
and supported arm that fails makes the command exit nonzero after preserving
the partial evidence. Declared unsupported optional arms do not fail the run.
A rerun never overwrites existing partial or final evidence.
A `.partial` path is never authoritative, including the crash cut after its
footer is durable but before final-name publication.

## Analysis

For each complete paired block:

`log_ratio = ln(evering_operations_per_second / baseline_operations_per_second)`

The point estimate is `exp(median(log_ratio))`.

The deterministic percentile bootstrap resamples complete paired blocks 10,000
times. Its seed is derived from the study seed and stable contrast encoding.
Screening reports descriptive 95% intervals.

For a focused family of `m` contrasts, each contrast uses the two-sided
Bonferroni bootstrap tails `0.05 / (2m)`, providing at least 95% familywise
coverage.

The practical-equivalence band is `[0.95, 1.05]`:

- interval wholly inside the band: practically equivalent;
- interval wholly above 1.05: candidate directionally faster;
- interval wholly below 0.95: candidate directionally slower;
- otherwise: inconclusive.

A sustained crossover is the first ordered payload whose complete interval
establishes one direction and whose next larger payload establishes the same
direction. The largest payload alone cannot establish a sustained crossover.

Analysis resamples blocks, never individual rows; rejects incomplete pairs,
mixed platforms, mixed targets, or changed focused families; never mutates raw
evidence; and produces deterministic Markdown with traceable artifact,
contrast, and block identities.

Latency ratios, combined coordinator-plus-worker CPU ns/op, logical GiB/s, and
kernel counters are secondary only when collected symmetrically. Instrumented
throughput, latency, and counter runs are separate when instrumentation changes
the primary path.

## Mechanism measurements

Mechanism evidence is separate from IPC trials. Registered operations are:

- reserve/publish;
- claim/recycle;
- shared allocate/release;
- notification signal/consume;
- one complete process exchange.

Each row names whether it is same-process, cross-thread, or cross-process,
records exact iterations and state reset, and includes a non-elided control
loop using `std::hint::black_box`. Net and gross costs are reported; negative
subtracted time is not manufactured into zero or a positive result.

Mechanism rows cannot be decoded or reported as whole-system throughput. An
association between mechanism and process evidence supports only a bounded
explanation.

## Recovery experiment

Recovery is correctness evidence, not a throughput sample. One fresh worker is
terminated at each registered cut:

- before publish;
- after publish;
- after claim;
- before recycle.

After terminal `Exit` from the exact retained `Supervisor`, the coordinator
admits death, rechecks and drains authoritative shared state once, repairs,
reaps, and classifies each accepted operation.

Valid recovery satisfies:

`accepted = validated + recovered_loss`

Duplicate and fabricated records are zero. Timeout, notification error, pipe
closure, PID, or heartbeat never authorizes death admission. Recovery records
the cut, exact exit status, counts, duration, repaired layouts, quarantined
bytes, and success of a subsequent clean attach/run.

## Invalidity rules

A trial has no performance value when:

- phase boundaries differ from the declared arm;
- requested/accepted/completed/validated conservation fails;
- a response number or payload is wrong, duplicate, or fabricated;
- the child is not exactly waited or killed-and-waited;
- a phase exceeds its absolute deadline;
- resource, queue, or task growth exceeds its declared bound;
- fallback, timeout, comparator error, or unsupported behavior is encoded as
  elapsed success;
- metadata, schedule, arm pair, environment, or revision is absent or
  inconsistent;
- the operation count/order cannot be reproduced;
- instrumentation changes one arm only.

Invalid and unsupported rows remain in partial evidence with a reason and no
elapsed performance value. No post-hoc exclusion is permitted.

## Comparator admission

The mandatory baseline is the real two-process framed IPv4-loopback stream.
Platform-native Unix-domain sockets and Windows named pipes are separate
transport factors, not aliases of the portable baseline.

Optional whole-system admission order is current `shmipc` on Linux, then
iceoryx2 only for a named cross-platform middleware question. Every admitted
row records exact version, runtime, geometry, capacity, batching, allocation,
and fallback behavior. Fallback is invalid or a separately named arm.

Monoio is a runtime-driver candidate, not an IPC comparator. It may enter a
separate Linux/macOS study only over the same transport, topology, connection
count, work, validation, and bounds. No optional dependency is added before a
written equivalence/admission review.

## Evidence retention and reporting

Pilot and unreferenced exploratory artifacts remain ignored. Every artifact
cited by `benchmarks/report.md` is copied unchanged into
`benchmarks/evidence/`, revalidated there, and committed with the report. If a
cited artifact exceeds 5 MiB, publication stops until a content-addressed
external archive is approved.

Each report claim names artifact, revision, platform, target, topology,
payload, capacity, in-flight, candidate/baseline policies, extent/geometry,
block count, estimator, familywise interval, and decision. Negative, null,
unsupported, invalid, and inconclusive results remain visible. No claim
generalizes beyond its recorded conditions.

## Core completion

Core completion requires:

- evidence v2, deterministic paired scheduling, absolute lifecycle deadlines,
  process-crash persistence, and complete validation;
- matched Evering busy/adaptive/notified and blocking-stream arms;
- native registered analysis;
- mechanism and recovery evidence;
- 10-block screening and 30-block focused families on Windows and Tumbleweed;
- a checked-in condition-qualified report and every cited evidence artifact.

Optional comparators, runtime drivers, latency distributions, process counters,
plots, and hosted regression tracking are not core-completion requirements.
