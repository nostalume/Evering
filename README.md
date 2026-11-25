<div align="center">

# Evering

[![CI](https://github.com/nostalume/evering/actions/workflows/ci.yml/badge.svg)](https://github.com/nostalume/evering/actions/workflows/ci.yml)

</div>

Evering is an experimental Rust substrate for bounded inter-process communication over
shared memory. A region holds typed layouts, a pool hands out recoverable blocks, a
channel moves one block between two members of a session, and a notification object says
when to look again. None of that needs an operating system: mapping an extent, ringing a
doorbell, and handing resources to a child are adapters you can supply yourself.

## What it is for

- **Transfer that survives a dead peer.** A payload occupies a Pool block with one
  reclamation authority at a time, so a peer that dies mid-flight is reclaimed from shared
  state instead of guessed at, and a failed send hands the same input back to its sender.
- **Typed payloads, checked on arrival.** A block carries the schema it claims, and the
  receiver either admits that body or keeps its claim untouched; nothing is reinterpreted.
- **Bounded by construction.** Queue capacity, pool extent, block classes, and the framing
  of a handover are fixed when they are created and validated at every entrance.
- **Nothing process-local in shared bytes.** Pointers, native handles, wakers, and local
  reference counts stay in your own process, which is what lets a second process read the
  same region without translating anything.

## The transfer path

[`examples/index.rs`](examples/index.rs) is the full program; this is its publish and admit
pair, with the error mapping, the per-peer slot indexing, and the run's budget timeout
elided.

```rust
let pool = session.create_pool(8 * 1024 * 1024, None)?;
let (jobs, job_port) = session.create_channel(4)?;
let (jobs_tx, jobs_rx) = jobs.split();

// Publish: reserve a slot, fill a Pool block, commit it.
let permit = signals.reserve(&jobs_tx).await?;
let block = pool.as_ref().copy(&body)?;
permit.send(block.encode(JOB))?;

// Admit: claim, then name the body the payload claims to carry.
let claim = signals.claim(&jobs_rx).await?;
let committed = signals.admit::<[u8]>(claim, pool.as_ref(), JOB)?;
```

`job_port` is a fixed number of bytes, so it reaches the peer through a handover rather
than through the region: `os::Handoff::bind()` on the parent, `os::Handoff::receive()` in
the child, together with the mapping and one doorbell. Each side splits its own `Tx` and
`Rx` from the role it was admitted with, and the order to finish in is close the
transmitters, drain the peers, then remove the channels.

## Try it

```console
cargo run --example index --features os,tokio -- README.md Cargo.toml
```

prints bytes, lines, and a checksum for each file. It runs two workers as real processes:
failure is reported and supervised, abrupt in-flight work is not replayed, and a worker
that never answers ends the run at its budget instead of leaving the coordinator parked on
a doorbell nobody will ring.

The second example sends the same three compile jobs twice — once to a daemon in another
process, once to a matched framed-TCP server — then retires the peer and reclaims what it
left behind:

```console
cargo run --release --example compile_cache --features os,tokio
```

Release is the intended profile, because the example's own digest over a 1 MiB payload
dominates a debug build, and the TCP arm is deliberately favourable: its server is a thread
inside the parent, while the Evering peer crosses `exec` and owns the other process. Add
`--geometry` to see the reservation each exact block range asks for, which is a floor set by
the largest class rather than by traffic. Which of these programs CI executes, and on which
host, is recorded in [stability](docs/stability.md).

## Install

Version `0.1.0` is on crates.io, and its public ABI stays experimental:

```toml
evering = { version = "0.1.0", features = ["os", "tokio"] }
```

The default feature is `std`. `map`, `notify`, and `process` are composable and `os`
selects all three; `tokio` adds process-local waiting; `tracing` adds no capability, only
the `DEBUG` events that name a declined reap's exit and each mutation of the shared heap.
The core also builds with default features off, which is
[what a `no-std` build gets](docs/stability.md#what-a-no-std-build-gets).

The crate builds on stable Rust and needs no nightly.
[`rust-toolchain.toml`](rust-toolchain.toml) pins the version these checks were run on,
which is also the `rust-version` in [`Cargo.toml`](Cargo.toml), and `rustup` installs it on
first use; a check run through `+nightly` compares a different compiler than CI does.

## Limits

Peers are assumed to agree on build, protocol representations, and session composition.
Recovery of shared ownership needs exact participant-death evidence; ambiguous general-heap
mutation is stopped rather than reconstructed; application work is not replayed; and no
cross-build Rust ABI is promised. Reading a region without joining it is offered, as a
standpoint outside the roster rather than as a kind of membership.

## Documentation

- [Evering Architecture](docs/architecture.md)
- [What a surface here can be relied on for](docs/stability.md)
- [Which byte facts are published](docs/abi.md)
- [Placing a region that is already in your image](docs/porting.md)
- [Evering Agent Guide](docs/AGENTS.md)

Each page states one kind of statement and one kind only; the rule that assigns them
is stated in [the agent guide](docs/AGENTS.md).

## License

[Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
