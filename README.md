<div align="center">

# Evering

[![CI](https://github.com/nostalume/evering/actions/workflows/ci.yml/badge.svg)](https://github.com/nostalume/evering/actions/workflows/ci.yml)

</div>

Evering is an experimental Rust substrate for bounded inter-process
communication through shared memory. Its operating-system-independent core
combines relocatable typed layouts, recoverable transfer storage, bounded duplex
queues, and explicit ownership transitions. Native mapping, process handoff,
notification, and asynchronous waiting are additive adapters.

## Model

- Each persistent layout records and validates its own schema and immutable
  information; there is no session-wide manifest.
- A generational Directory owns reusable Channels and Pools.
- Pools use authoritative lifecycle words for transferable Blocks. Talc remains
  isolated to Directory construction and explicit `PBox` values; it is never a
  silent transfer fallback.
- A Channel is a protocol-typed duplex pair whose admitted role derives the only
  valid transmit and receive directions.
- Queue state is authoritative. Signals only advise a peer after commit and
  optionally wait before retrying shared truth.
- Pointers, native handles, wakers, callbacks, and process-local reference counts
  never represent shared ownership.

The fast path keeps protocol, queue, and layout behavior concrete for
monomorphization and inlining. Construction policy disappears after admission.

## Use from Git

Evering is not published yet. Pin the repository while its public ABI remains
experimental:

```toml
evering = { git = "https://github.com/nostalume/evering", features = ["os", "tokio"] }
```

The default feature is `std`. `map`, `notify`, and `process` are composable;
`os` selects all native adapters. `tokio` adds process-local waiting. The core
also checks with default features disabled.

## Practical example

The indexer creates shared storage and typed Channels, hands native resources to
two worker processes, admits runtime-classified results, and closes and removes
its resources:

```console
cargo run --example index --features os,tokio -- README.md Cargo.toml
```

It prints bytes, lines, and a checksum for each file. Worker failure is reported
and supervised, but abrupt in-flight application work is not replayed.

## Benchmark evidence

The registered [IPC study](benches/README.md) compares complete two-process
request/response work against matched framed TCP and Unix-domain streams. Setup
is outside timing; payload transformation, response validation, capacity,
in-flight bounds, and trial admission are matched. The result measures the
whole transport path, not an isolated ring instruction.

The newest complete focused Tumbleweed delivery study compares one persistent
Evering connection with UDS readiness. Both arms read every request byte and
return the same fixed-size digest; all focused trials conserved and validated
their work:

| Payload bytes | Evering / UDS ratio | Paired 95% interval |
| ---: | ---: | ---: |
| 0 | 3.097 | [2.767, 3.341] |
| 1,024 | 3.064 | [2.953, 3.197] |
| 65,536 | 2.934 | [2.879, 3.136] |

These are condition-specific throughput decisions, not universal IPC claims or
causal attribution. A later Windows diagnostic found that matched 64-KiB Pool
allocation/copy metadata was not the observed end-to-end bottleneck; short-lived
setup dominated wall time, while sustained residual costs remain unattributed
between payload work and cross-process scheduling. The study document records
the conditions, claim levels, conservation checks, historical evidence,
analysis commands, and extension rules. Plots are generated on demand rather
than committed as evidence.

## Limits

Evering currently assumes peers agree on build, target architecture, protocol
representations, and session composition. It does not promise cross-build Rust
ABI compatibility, read-only participation, automatic application replay, or
recovery of ambiguous general-heap mutation. Exact participant-death evidence
is required before shared ownership recovery.

## Documentation

- [Architecture and invariants](docs/architecture.md)
- [Technology and agent guide](docs/AGENT.md)
- [Benchmark method and evidence](benches/README.md)

## License

[Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
