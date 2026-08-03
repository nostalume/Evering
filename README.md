<div align="center">

# Evering

[![CI](https://github.com/nostalume/evering/actions/workflows/ci.yml/badge.svg)](https://github.com/nostalume/evering/actions/workflows/ci.yml)

</div>

Evering is an experimental Rust substrate for bounded inter-process communication
through shared memory. Its operating-system-independent core combines relocatable
typed layouts, recoverable transfer storage, bounded duplex queues, and explicit
ownership transitions. Native resources and asynchronous waiting are adapters.

## Model

| Concept | What you use it for |
| --- | --- |
| `Session` | Create or open one typed view of a shared-memory region. |
| `Pool` | Store transferable payloads in bounded, recoverable blocks. |
| `Channel<H>` | Exchange fixed-representation headers through a bounded duplex queue. |
| `Port<H>` | Hand one channel role to another process as stable bytes. |
| `Signals` | Wait for progress and notify a peer after shared state commits. |

A creator creates a `Session`, `Pool`, and channel, then hands the mapping,
`PoolId`, and `Port` to a peer. The peer opens the session and adopts the port.
Each side splits its channel into `Tx` and `Rx`; sending publishes a Pool-backed
transfer, and receiving admits its schema into a typed block. Close transmitters,
drain peers, then remove the channels.

Native resources and Tokio waiting are optional adapters. `Session::heap` and
`PBox` are an explicit general heap, never an automatic channel fallback. The
[architecture](docs/architecture.md) defines recovery and safety invariants.

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

The indexer hands shared storage and typed Channels to two workers, admits their
runtime-classified results, then closes and removes its resources:

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

These are condition-specific throughput results, not universal IPC claims or
causal attribution. A later Windows diagnostic excluded matched 64-KiB Pool
allocation/copy metadata as the observed bottleneck but did not attribute the
remaining sustained costs. The study records conditions, conservation checks,
analysis, and extension rules; plots are generated on demand.

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
