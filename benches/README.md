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
attributes the effect to one queue, allocator, or notification operation.

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
inadmissible prefix; reruns never overwrite an existing artifact.

Retained historical screening evidence is available for audit:

- [Tumbleweed TCP screening, 2026-07-31](evidence/core-ipc-tumbleweed-screening-20260731.jsonl)
- [Tumbleweed UDS screening, 2026-07-31](evidence/local-ipc-unix-tumbleweed-screening-20260731.jsonl)
- [Tumbleweed UDS screening, 2026-08-02](evidence/local-ipc-unix-tumbleweed-screening-20260802.jsonl)

Those artifacts describe earlier implementations and must not be pooled with
the focused result or treated as the current performance baseline.

## Next evidence

The present paper supports one conditioned throughput decision and one bounded
allocator rejection. The most useful extensions are:

1. repeat the focused UDS experiment on native Linux and another physical host;
2. repeat it on native Windows with a named-pipe baseline;
3. collect symmetric CPU time, context switches, page faults, and cache counters
   in a separate instrumented run;
4. register latency and multi-client studies rather than deriving them from
   throughput;
5. admit additional IPC systems only after matching topology, work, bounds,
   allocation, batching, fallback, and validation semantics.

Monoio is a runtime-driver candidate, not an IPC comparator. `shmipc` or another
shared-memory system may become a comparator only through an explicit
equivalence review; otherwise its result would answer a different question.
