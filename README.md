<div align="center">

# Evering

</div>

Evering is a Rust substrate for bounded inter-process communication through
shared memory. It combines relocatable typed layouts, recoverable storage
capabilities, bounded duplex queues, explicit ownership transfer, and optional
operating-system notification/process adapters.

The core is operating-system independent. Platform sources own mapping and
resource exchange; async runtimes may drive advisory waits without becoming
part of shared protocol state. Talc remains an isolated general heap for
Directory layouts and explicit `PBox` values, while transferable messages use
recoverable Pools with authoritative lifecycle words.

The project is experimental: same-build shared ABI, recovery boundaries, and
performance methodology are actively validated. It is not yet a stable IPC
framework.

## Evidence

The [IPC study](benches/README.md) compares complete two-process Evering
request/response work with matched framed TCP and Unix-domain streams. Raw
JSONL, generated Markdown, and SVG artifacts are retained beside the method.
Screening results are descriptive and do not authorize a universal performance
claim.

## Documentation

- [Architecture](docs/architecture.md)
- [Agent goal and technology guide](docs/AGENT.md)
- [Benchmark method and report](benches/README.md)

## Contributing

Bug reports and contributions are welcome through
[GitHub issues](https://github.com/nostalume/evering/issues).

## License

[Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
