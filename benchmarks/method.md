# Evering IPC study method

This document defines the experiment before performance results are collected.
It is a validity contract, not a claim that Evering is faster or slower than
another transport.

## Question

Under which declared payload, queue, concurrency, waiting, memory, and platform
conditions does an Evering request/response channel change latency, throughput,
CPU demand, or recovery behavior relative to a maintained OS transport?

The study separates two questions:

1. What does each Evering mechanism cost in isolation?
2. What does a complete two-process request/response workflow cost under
   matched logical work?

An observed difference is attributed to a mechanism only when an isolated
factor and an appropriate counter change together. Otherwise it is reported as
an implementation-level difference under the tested conditions.

## Logical operation

One accepted operation consists of exactly one request record containing:

- a monotonically assigned operation number;
- the declared payload length;
- deterministic payload bytes derived from the study seed and operation
  number.

One completed operation consists of exactly one response carrying the same
operation number and a deterministic transform of the request payload. A
completed operation becomes validated only after the coordinator checks the
entire response. Sampling bytes is not valid evidence.

Every cell must report identical `accepted`, `completed`, and `validated`
counts. A mismatch invalidates the cell; it is never recorded as slow success.

## Process and timing boundary

The process study always uses one coordinator process and one worker process.
Threads or tasks inside either process are a declared factor. An
implementation that cannot support the selected topology is recorded as
unsupported.

Excluded setup:

- resource creation, mapping, and typed admission;
- child spawn and handle exchange;
- runtime construction and registration;
- allocation of fixed study buffers;
- warm-up operations;
- start-barrier arrival.

The clock starts when both processes have completed warm-up and the coordinator
releases the start barrier. It stops after the coordinator has validated the
last of the exact requested operations. Queue contention, backpressure,
notification, request allocation, response allocation, and payload copying
belong inside this interval when the selected implementation requires them.

Excluded drain:

- cooperative channel close;
- remaining notification consumption;
- child wait or forced termination;
- unmapping and resource destruction;
- result serialization.

Setup, timed work, and drain failures have distinct statuses. A failed setup or
drain cannot produce a timed sample.

## Factors

The screening matrix varies:

| Factor | Screening values |
| --- | --- |
| Platform | Windows, openSUSE Tumbleweed |
| Implementation | Evering, one maintained OS baseline |
| Waiting policy | busy, adaptive, OS-notified |
| Payload bytes | 0, 64, 1 KiB, 16 KiB, 64 KiB |
| Queue capacity | 1, 8, 256 |
| In-flight operations | 1, 8, 64 |
| Operations per trial | fixed exactly by the cell |
| Trial order | seeded randomized blocks |

Unsupported implementation/policy pairs are explicit cells. They are not
silently replaced by another policy. A focused matrix may reduce factor values
only after screening, and its selection must be recorded before focused trials
run.

The memory extent is derived from the payload, capacity, and fixed protocol
overhead, then recorded. It is not selected independently per implementation.
Automatic allocator geometry is used unless allocator geometry is itself the
declared factor.

## Evering waiting policies

- **busy** retries the same nonblocking shared operation without an OS wait;
- **adaptive** performs a recorded bounded spin/yield phase, then uses the same
  OS notification path as notified mode;
- **OS-notified** retries shared state, waits on the directional notification,
  clears it, and immediately retries authoritative shared state.

All policies use the same shared layouts, payload representation, operation
numbering, validation, and close behavior. Notification remains advisory and
does not alter the shared schema.

## Baseline admission

A baseline is admitted only when it is maintained on the tested platform and
can implement the same two-process topology, exact operation count, payload
validation, backpressure bound, and timing boundary. Its source revision,
configuration, and unsupported cells are recorded.

The historical `shmipc` Git revision and Monoio comparison are not admitted:
their APIs are stale and the old runner changes transport, process topology,
runtime, connection count, and queue policy simultaneously. They may be
reintroduced only as newly reviewed baselines; old numbers are discarded.

The first portable OS baseline is a framed TCP byte stream over numeric IPv4
loopback on every supported hosted platform. It measures one portable
kernel-mediated transport, not a competing shared-memory library. Unix-domain
sockets and Windows named pipes remain separate, platform-specific factors;
they must not be substituted into this baseline under the same transport name.

## Trial order and stopping

Each block contains one trial for every supported cell selected into that
block. A recorded seed deterministically shuffles cell order within every
block. Trial count is fixed before execution; results do not stop when a
preferred confidence interval or ranking appears.

Warm-up count, measured operation count, blocks, seed, adaptive-spin bound, and
timeouts are command inputs and are copied into raw evidence. Dividing work
among producers uses quotient plus remainder, so the sum is always the exact
requested count, including counts smaller than producer count.

## Raw evidence

Raw evidence is append-only tab-separated text with a format version. One file
starts with one metadata row followed by trial rows in actual execution order.
Fields contain no tabs or newlines.

Metadata records:

- format version and generating command;
- source revision and dirty state;
- target triple, operating system, architecture, Rust version;
- wall-clock start, seed, warm-up count, timeout, and block count.

Trial records:

- block and order within block;
- implementation and waiting policy;
- payload, capacity, in-flight bound, memory extent;
- requested, accepted, completed, and validated operation counts;
- elapsed nanoseconds and terminal status;
- setup, timed, or drain error text when present.

The validator rejects unknown format versions, missing metadata, duplicate
block/cell pairs, impossible counts, zero elapsed time for successful measured
work, unsupported cells carrying timings, and errors recorded as success.
Raw rows are never deleted by analysis.

## Measurements and analysis

Every supported process cell records wall time and derives operations per
second. Platform tools may additionally collect process CPU time, voluntary and
involuntary context switches, syscalls, page faults, and peak resident memory.
Unavailable counters remain absent; they are not synthesized.

Kernel microbenchmarks separately measure:

- reserve and publish;
- claim and recycle;
- shared allocation and release;
- synchronous adapter fast-path wrapping;
- one doorbell notification syscall.

Analysis reports per-cell median and distribution-free bootstrap confidence
intervals, paired within-block ratios against the baseline, and crossover
tables over payload and in-flight count. It retains regressions, null results,
unsupported cells, and invalid trials. Claims cite the raw file, source
revision, seed, exact cell, and interval.

No result from one platform, payload, policy, concurrency, or memory extent is
generalized beyond that condition. A causal explanation requires agreement
between the isolated microbenchmark and relevant process counters.

## Recovery experiment

Recovery is a correctness study, not a throughput sample. The coordinator
records accepted operation numbers, terminates the exact retained worker at a
seeded transition, waits for terminal status, explicitly admits death, repairs,
and classifies every accepted operation as validated, recovered loss, or
protocol failure.

The invariant is conservation:

`accepted = validated + recovered loss`

No timeout, notification error, pipe closure, or PID observation authorizes
recovery. Recovery trials report transition, seed, exact exit status, repaired
layouts, conservation counts, and any quarantined allocation.

## Invalidity rules

A trial is invalid when any of these occurs:

- setup/timed/drain boundaries differ from the declared implementation path;
- accepted, completed, or validated work differs from the requested count;
- a response operation number or payload is wrong or duplicated;
- the child is not waited;
- resource, queue, or task growth exceeds its declared bound;
- timeout or comparator error is converted to elapsed time;
- the environment or source revision is missing;
- trial order cannot be reproduced from the recorded seed;
- a counter collection mode changes only one implementation's workload.

Invalid and unsupported rows remain in the raw file with no performance value.
