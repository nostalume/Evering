# Evering IPC performance study

## Abstract

This study measures a complete two-process request/response operation, not an
isolated queue instruction. It compares Evering shared-memory channels with a
framed stream while matching logical work, application-visible capacity,
in-flight concurrency, process topology, validation, and trial lifecycle.

The newest complete focused experiment ran on openSUSE Tumbleweed under WSL2.
For empty, 1-KiB, and 64-KiB requests, Evering delivered respectively 3.097,
3.064, and 2.934 times the validated throughput of a Unix-domain stream (UDS).
All familywise intervals lie above the registered 1.05 practical threshold.
The result applies to the measured persistent one-coordinator/one-worker digest
workload; it neither proves that shared memory is universally faster nor
attributes the effect to one queue, allocator, or notification operation. A
later instrumented re-run of the same schedule does attribute it, within bounds
that the Attribution section states: at 0 B and 1 KiB, 97.7--98.6% of that
re-run's own ratio is a difference in coordinator CPU time inside the timed
window, and the kernel entries the shared-memory path no longer makes carry
between 67% and 79% of that difference in log terms.

## Result

The focused experiment used 15 paired blocks per payload and completed all 90
trials. Capacity and maximum in-flight work were both eight, the shared extent
was 32 MiB, and the Evering arm used adaptive waiting. The worker read every
request byte and returned an eight-byte digest.

| Payload | Evering op/s | UDS op/s | Evering / UDS | Familywise interval | Decision |
|---:|---:|---:|---:|---:|---|
| 0 B | 1,319,071 | 420,043 | 3.097 | [2.767, 3.341] | Faster |
| 1 KiB | 1,119,129 | 365,510 | 3.064 | [2.953, 3.197] | Faster |
| 64 KiB | 170,705 | 57,774 | 2.934 | [2.879, 3.136] | Faster |

Every trial conserved
`requested = accepted = completed = validated`. Accounted phases totalled
7.275 s of setup and warmup, 20.365 s of timed work, and 0.185 s of drain.
Individual timed phases lasted 156.1--320.3 ms. The evidence identity is
`6103c79dfe62c5fb3a790f8b961e0f12338e2fa4fcb540210a2165c0fd8a1992`.

The practical conclusion is narrow: Evering was about three times faster than
UDS for these three persistent-connection conditions on this host. The
experiment does not estimate latency tails, CPU or energy efficiency, equal
memory efficiency, multi-producer scaling, or crash-recovery cost.

## Question and metric

For condition `c`, the primary estimand is

`R(c) = throughput(Evering, c) / throughput(stream, c)`

where throughput is the number of fully validated operations divided by timed
seconds. A ratio exists only for a complete pair of successful trials. Results
are not pooled across payloads, platforms, transports, waiting policies, or
memory geometries.

One operation consists of a numbered deterministic request, a complete worker
read and digest transform, and an exactly matching response. A submitted or
accepted request is never counted as completed. Wrong, missing, duplicated, or
fabricated responses invalidate the trial.

This is a deployment-level metric: it includes allocation and copying required
by each arm, transport or publication, synchronization, worker processing,
response transfer, and validation. Setup, process creation, mapping, handle
exchange, warmup, drain, child reaping, and evidence persistence are measured
separately and excluded from throughput.

## Compared systems

### Evering

The coordinator and worker share one bounded bidirectional channel and one
recoverable Pool. A request is copied into a Pool allocation and published as a
typed envelope. The worker admits the allocation, reads the bytes, writes the
digest, and returns ownership through the channel. The coordinator admits the
returned allocation, verifies the digest, and releases it.

The `busy`, `adaptive`, and `notified` policies change retry behaviour only.
Notification is advisory: authoritative shared state decides whether a send,
receive, close, or recovery transition occurred.

### Stream baselines

The portable baseline is framed IPv4-loopback TCP with `TCP_NODELAY`. The Unix
comparison reported above uses UDS with the same framing and logical state
machine. A request frame carries an operation number, payload length, and full
payload; the response carries the operation number and digest. The coordinator
uses Tokio readiness when nonblocking I/O stalls, while the worker uses
blocking I/O.

The arms match service semantics and resource bounds, not physical byte
movement. Kernel transport, framing, copying, allocation, and readiness are
real costs of the stream implementation and therefore belong in the
whole-system comparison.

## Experimental design

The topology is one persistent coordinator, one fresh worker per trial, and one
channel or connection. Each paired block observes both arms. A fixed seed
randomizes condition order and arm order without changing membership or work.
Fresh mappings, channels, connections, and worker processes limit cross-trial
state; the coordinator and host can retain cache, allocator, scheduler, and
thermal history.

Each trial has six phases:

1. Setup creates resources, exchanges handles, and starts the worker.
2. Warmup performs the same logical operation used during measurement.
3. A reserved readiness exchange proves both endpoints admitted the condition.
4. Timed work executes the frozen operation count under bounded backpressure.
5. Drain closes resources and observes one exact child terminal state.
6. Evidence is persisted after timing.

One absolute deadline covers setup, warmup, measurement, drain, and child reap.
A retry cannot restart the deadline. Before evidence collection, an excluded
pilot chooses a fixed count for each condition and arm so measurement is long
enough to reduce timer and startup sensitivity without allowing one command to
run unboundedly. The chosen count is frozen before the first paired block.

Capacity is the maximum application-visible outstanding record count.
In-flight is the maximum admitted but incomplete operation count. Their minimum
is the logical sliding window, but they are not physically interchangeable:
changing Evering capacity also changes ring geometry, whereas changing
in-flight alone does not.

## Statistical analysis

For every complete paired block, analysis computes

`log_ratio = ln(Evering operations/s / baseline operations/s)`.

The point estimate is the exponentiated median log ratio. A deterministic
percentile bootstrap resamples complete blocks 10,000 times; it never resamples
individual operations. For `m` focused contrasts, two-sided bootstrap tails use
`0.05 / (2m)`, giving at least 95% familywise coverage.

The registered practical-equivalence band is `[0.95, 1.05]`:

- an interval wholly above 1.05 is Faster;
- an interval wholly below 0.95 is Slower;
- an interval wholly inside the band is Practically equivalent;
- every overlapping interval is Inconclusive.

Screening intervals are descriptive and never receive those decisions.
Incomplete schedules, mismatched conditions, duplicate families, and
unsupported evidence formats are rejected before analysis.

## Bottleneck evidence

A later Windows diagnostic separates short-session overhead from the timed
transport path. Five smoke observations completed in 4.838 s. Setup took
450--783 ms per case, while measured transfer took 0.10--45.10 ms. This shows
that repeatedly creating a worker and connection dominates short benchmark
commands, but setup is outside the persistent-connection throughput estimand.

A matched 64-KiB allocation experiment used three AB/BA pairs of 19,828
operations. Shared-Pool batches took 30.52--31.26 ms; a local-copy control took
31.68--32.48 ms. The paired median difference was -58.377 ns/op with interval
[-61.474, -55.679] ns/op. Under that diagnostic, Pool allocation metadata
cannot explain the observed roughly 140-us/message system cost.

This is a rejection, not a causal decomposition. Payload generation, worker
mutation, validation, cache traffic, notification, and cross-process scheduling
were not isolated symmetrically. The system and mechanism observations also
have different source identities, so they cannot be joined into a causal
estimate.

## Attribution

The question is whether the boundary crossings the shared-memory path removes
account for the measured ratio, and for how much. Two artifacts answer it, and
neither is the registered one.

The first re-runs the registered schedule with the boundary counters enabled:
`focused local-ipc-unix`, seed 20260802, 15 paired blocks, 90 trials, capacity
and in-flight eight, one coordinator and one worker, 32 MiB, adaptive waiting.
Every admitted row then also carries the coordinator's own run time over the
timed window, its two context-switch counts, and the worker's separate
accounting over its own bracket. That artifact is
[local-ipc-unix-tumbleweed-focused-instrumented-20261122.jsonl](evidence/local-ipc-unix-tumbleweed-focused-instrumented-20261122.jsonl)
(106,227 bytes, BLAKE3
`d8ec02a6d77f19eefe8d7c8d29b6a5b1505d740d91e2fd3f13fda35822ad2905`, identity
`aacee3457609aa68cc1a626199427b365e0f6a19b389fc8ca4fa4ba294684f7f`), source
`8463814`, rustc 1.100.0, Linux 6.18.58.1-microsoft-standard-WSL2. Its tree is
recorded dirty only by untracked local logs under `.codex/evidence`; no tracked
file differs from that revision.

Its point estimates are 4.584, 3.896, and 3.285 — above the registered 3.097,
3.064, and 2.934. The difference sits mostly in the baseline: its rate is 30%,
28%, and 14% below the registered one at the three payloads, while Evering's is
4% above at 0 B and 7% and 12% below at 1 KiB and 64 KiB. The registered
evidence carries no counters at all. Nothing below is therefore
quoted against the registered figures, and the registered decision stands as
recorded above; this section attributes the re-run's own ratio.

| Payload | Evering CPU µs/op | UDS CPU µs/op | Δ µs/op | Evering window µs/op | UDS window µs/op | UDS window minus UDS CPU µs/op | CPU share |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 B | 0.6751 | 3.0450 | -2.4057 | 0.7298 | 3.4108 | 0.3658 | 97.7% |
| 1 KiB | 0.8586 | 3.3190 | -2.4116 | 0.9654 | 3.8031 | 0.4841 | 98.6% |
| 64 KiB | 5.8447 | 13.3958 | -7.6276 | 6.6167 | 20.1062 | 6.7104 | 74.6% |

Δ is the median of the per-block differences. The CPU and window columns are
medians of per-operation figures, and the last column is the baseline's timed
window less its own run time: time inside the measured span in which the
coordinator process held no CPU. CPU share is the CPU ratio's log share of the
window ratio, so it is the part of the ratio that the two arms' run times
account for and the part that is waiting. Every row resolves run time in
nanoseconds. Preemption is negligible but not absent: no counter read records
more than four involuntary switches, against trials of 11,160 to 291,304
completed operations.

At 0 B and 1 KiB, then, the ratio is a CPU-time ratio: the remaining waiting
multiplies it by only 1.04 and 1.02. At 64 KiB it is not entirely one: the
baseline leaves 6.7 µs/op of its window off a CPU, its voluntary switches rise
to 0.287/op against Evering's 0.004, and a quarter of that ratio is the stream
arm waiting on the socket rather than doing work. The worker's own
figures say the same thing from the other end of the channel — 0.8907 µs/op of
run time for Evering against 3.2478 µs/op for UDS at 1 KiB, and 5.9321 against
17.5208 at 64 KiB — so the saving is present in both processes and is not an
artifact of how one of them was scheduled. Those worker figures are paired with
the rows above and never added to them, because a worker's bracket runs past the
coordinator's timed window.

The second artifact counts the kernel entries themselves, which no instrumented
row can do: a focused run cannot be traced, because its operation counts are
calibrated to fill the timed window and a trace inflates every syscall-bound
trial. Entries were therefore counted in a separate pool of ten traced
`smoke local-ipc-unix` runs, 1,010 operations per arm, at the registered 1 KiB,
capacity 8, in-flight 8 condition — the only condition both artifacts cover.

| Per operation, marker-delimited window | Evering | UDS |
|---|---:|---:|
| coordinator entries | 2.107 | 6.016 |
| worker entries | 2.055 | 6.018 |
| coordinator doorbell writes | 2.000 | — |
| coordinator socket sends and receives | — | 3.925 |
| coordinator readiness waits and self-wakes | 0.108 | 2.091 |

Evering's structure is exact rather than averaged: 2.000 doorbell writes per
operation in each process, and its waiting branch runs at 0.038 `epoll_wait`
entries per operation — it fires on under 4% of them. The pair makes 12.03
entries per operation for UDS against 4.16 for Evering, so 65% of the kernel
entries are gone, roughly half of the removal being the baseline's readiness
loop and half its socket copies.

The answer, in three parts:

- The ratio is removed boundary *work* to the extent that it is a CPU-time
  ratio at all: 97.7% and 98.6% of it in log terms at 0 B and 1 KiB, 74.6% at
  64 KiB.
- The removed *entries* carry between 67% and 79% of that CPU difference in log
  terms. The range is the honest form of the figure. A trace inflates the
  waiting branch — the baseline's own arm counters record 0.925 blocks per
  operation under the trace against 0.0047 voluntary switches per operation
  untraced — so the traced entry ratio of 2.89 over-states the steady-state
  count difference, and the structural comparison that does not depend on the
  waiting branch (4.000 doorbell writes across the pair against 9.932 socket
  sends and receives, a ratio of 2.48) under-states it.
- What is left over, 21% to 33% of the CPU difference in log terms, is
  per-entry cost or work no counter names. The removed classes are a 1 KiB
  socket copy and a blocking `epoll_wait`; the kept class is an 8-byte eventfd
  write. Putting the two figures beside each other as far as they will go makes
  the same point: if each entry cost what a baseline entry costs on average, the
  entry cut would predict 1.162 µs/op of coordinator run time for Evering
  against the 0.859 µs/op measured, so the arm that kept fewer entries also
  reports less run time per entry kept. Nothing here prices either class: this
  host exposes no performance counters under WSL2, so there is no
  cycles-per-operation figure for any arm, and no artifact holds a
  steady-state run time and an entry count for the same trial.

So removed boundary syscalls account for the direction of the measured effect
and most but not all of its size, and the part they do not account for is named
rather than assumed: it is per-entry cost, unmeasured.

These statements carry no decision. The registered `[0.95, 1.05]` band belongs
to the throughput ratio alone, and the second estimand is reported beside it as
an estimate, not a verdict. Their limits belong inside them: Linux only, since
the counters come from procfs and a Windows run legitimately reports their
absence; the counted condition is 1 KiB with capacity 8 and in-flight 8 only,
because the traceable scale carries one payload; the entry figures come from
101-operation trials whose timed windows are dominated by first touch of the
shared extent, which makes them counts and not costs; switch counts are a proxy
for blocking, not a syscall count; and the attribution is to the instrumented
re-run's ratio, which is not the certified registered one.

Both halves re-derive from the recorded commands: the counters are written
whenever the host exposes procfs, and `analyze` prints the second estimand
beside the first; the entries come from `strace -f -yy -e signal=none` over ten
`smoke local-ipc-unix` runs with `EVERING_MARKER_DIRECTORY` set, read by the
phase-marker counter retained from the stage 0.3c counting work.

That instrument is Linux's, and this paragraph is here to keep the claim inside
what it measures. The Windows arm is covered by the behavior its legs execute —
the same descriptor refused with the same words, the same receipt read off a pipe
instance — and no entry count is offered for it. The reason is not that counting
there is hard: a library-side call counter would be cheap, and would say how many
times evering asked rather than how many times the kernel answered. Putting the
two under one name would weaken the bounded attribution above rather than extend
it, so the estimand stays where the instrument is.

## Robustness and validity

The comparison admits a performance value only when:

- phase boundaries and logical work are identical in meaning;
- requested, accepted, completed, and validated counts are equal;
- every response number and digest is correct;
- both arms respect the same capacity and in-flight bounds;
- no timeout, fallback, or unsupported path is encoded as success;
- the worker reaches exactly one observed terminal state;
- the compiler, source, schedule, environment, and resource geometry are
  recorded and internally consistent.

Paired randomized blocks reduce temporal drift but cannot remove scheduler,
frequency, thermal, page-fault, virtualization, or background-load effects.
Threads are not assumed pinned and unavailable host controls remain explicit
limitations. Very short observations can still look precise; the per-arm pilot
and minimum duration are therefore admission requirements.

The shared-memory and stream arms intentionally differ in ownership and copy
paths. Internal memory use is not matched: socket buffers, private buffers,
mappings, Pool metadata, and runtime state have not been measured under one
common memory budget. Logical MiB/s is application payload throughput, not wire
or memory bandwidth.

The focused result is specific to x86_64 Tumbleweed under WSL2, the recorded
compiler and source, one coordinator and one worker, one persistent connection,
capacity and in-flight eight, a 32-MiB extent, adaptive waiting, and the digest
workload. Windows, bare-metal Linux, another architecture, concurrent clients,
open-loop arrivals, crash-heavy operation, and other transports require
separate evidence.

Execution reliability on that host carries two recorded limits, descriptive and not
admission rules. In the evidence written on 2026-11-18, a worker process failed to open
a `Pool` its parent had already published in 6 of 225 shared-memory trials and in none of
134 stream trials; the same failure appeared in a checkout of the published commit, and
eight hundred milliseconds of retries inside the worker did not clear it, so one
three-block screening — 69 shared-memory trials — completes roughly one time in seven.
Separately, the recorder discards the clause name when it rejects a measure, and it
validates before writing the row, so a rejected trial leaves no inspectable evidence of
what it rejected. A screening interrupted by either is an inadmissible prefix like any
other; neither limit licenses accepting a trial that failed admission.

## Recovery evidence

Recovery is tested as correctness, not throughput. A worker is terminated after
reserve, before publish, after publish, or after claim. Only an observed
terminal process exit authorizes repair; a timeout, notification error, closed
pipe, PID, or heartbeat does not prove death.

Valid recovery satisfies

`accepted = validated + recovered_loss`

with zero duplicate and fabricated records. The test also verifies channel
removal, reuse of a dead participant slot at a newer generation, and a clean
subsequent attach, exchange, and removal.

## Reproduction

Benchmark tests are isolated from the ordinary project test suite:

```console
cargo test --features benchmark --test ipc-study
```

Run a study and analyze complete evidence with:

```console
cargo bench --bench ipc --features benchmark -- <study-command>
cargo bench --bench ipc --features benchmark -- analyze <evidence.jsonl>...
```

The optional `plot` feature creates SVG from the admitted analysis without
changing estimates:

```console
cargo bench --bench ipc --features benchmark,plot -- \
  plot <new-output-directory> <evidence.jsonl>...
```

Plots are derived presentation artifacts and are not committed as authority.
The analyzer consumes newline-delimited JSON containing one header, ordered
trial rows, and one terminal record that binds the row count, schedule, and
digest of preceding bytes. Interrupted evidence remains an inspectable but
inadmissible prefix; reruns never overwrite an existing artifact. A file is
read against the family generation its header names, so a schedule written
before a retune reproduces from the definition that recorded it, and a revision
no retained artifact needs is refused rather than reinterpreted.

Retained historical evidence is available for audit:

- [Tumbleweed TCP screening, 2026-07-31](evidence/core-ipc-tumbleweed-screening-20260731.jsonl)
- [Tumbleweed UDS screening, 2026-07-31](evidence/local-ipc-unix-tumbleweed-screening-20260731.jsonl)
- [Tumbleweed UDS screening, 2026-08-02](evidence/local-ipc-unix-tumbleweed-screening-20260802.jsonl)
- [Windows core smoke, 2026-08-02](evidence/core-ipc-windows-smoke-20260802.jsonl)

Those artifacts describe earlier implementations and must not be pooled with
the focused result or treated as the current performance baseline. The last one
is what keeps the `core-ipc` v2 definition compiled: it records five rows
against a schedule that no longer has a producer, so the checkable claim is
that generation's own digest and heading rather than a reconstruction of it.

## Next evidence

The present paper supports one conditioned throughput decision and one bounded
allocator rejection. The most useful extensions are:

1. repeat the focused UDS experiment on native Linux and another physical host;
2. repeat it on native Windows with a named-pipe baseline;
3. add page faults and cache counters to the instrumented run, which already
   records symmetric CPU time and both context-switch counts; this host exposes
   no performance counters under WSL2, so the addition needs native Linux or
   another host;
4. register latency and multi-client studies rather than deriving them from
   throughput;
5. admit additional IPC systems only after matching topology, work, bounds,
   allocation, batching, fallback, and validation semantics.

Monoio is a runtime-driver candidate, not an IPC comparator. `shmipc` or another
shared-memory system may become a comparator only through an explicit
equivalence review; otherwise its result would answer a different question.
