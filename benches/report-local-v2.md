# IPC study

## local-ipc-unix/v2 — screening

Screening is descriptive; it authorizes no performance decision.

Environment: `linux` / `x86_64-unknown-linux-gnu`; host `os=linux;arch=x86_64;logical=20;cpu=13th Gen Intel(R) Core(TM) i5-13500HX;affinity=0-19;page=4096;power=unavailable;kernel=Linux 6.18.33.2-microsoft-standard-WSL2 #1 SMP PREEMPT_DYNAMIC Thu Jun 18 21:54:43 UTC 2026 x86_64 GNU/Linux;native=wsl;physical=unavailable;smt=unavailable;thermal=unavailable;background=unavailable;pilot=806da43385699a6c40510b72320d61e3c65466508d12489f68b32f7dfb955b96`; rustc `rustc 1.97.0-nightly (9eb3be26b 2026-05-18) binary: rustc commit-hash: 9eb3be26b46eccea1de7448ea9cc3c1d20bb1a35 commit-date: 2026-05-18 host: x86_64-unknown-linux-gnu release: 1.97.0-nightly LLVM version: 22.1.4`. Evidence: `e7b513ffc9699623e238d2263d5f7cb20f867614:9864694969635921446#0..3`. Limit: 90 s. Baseline: `uds/readiness`.

| arm | payload | capacity | in-flight | memory | blocks | effect | interval |
|---|---:|---:|---:|---:|---:|---:|---:|
| evering/adaptive | 0 | 8 | 8 | 33554432 | 3 | 2.938213 | [2.813674, 3.294607] |
| evering/adaptive | 64 | 8 | 8 | 33554432 | 3 | 2.644060 | [2.396848, 2.750308] |
| evering/adaptive | 1024 | 8 | 8 | 33554432 | 3 | 1.676745 | [1.622444, 1.842459] |
| evering/adaptive | 16384 | 8 | 8 | 33554432 | 3 | 1.132416 | [1.099599, 1.149312] |
| evering/adaptive | 65536 | 8 | 8 | 33554432 | 3 | 1.277236 | [1.203686, 1.307830] |
