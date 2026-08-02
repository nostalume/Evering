# IPC study

## core-ipc/v1 — screening

Screening is descriptive; it authorizes no performance decision.

Environment: `linux` / `x86_64-unknown-linux-gnu`; host `os=linux;arch=x86_64;logical=20;cpu=13th Gen Intel(R) Core(TM) i5-13500HX;affinity=0-19;page=4096;power=unavailable;kernel=Linux 6.18.33.2-microsoft-standard-WSL2 #1 SMP PREEMPT_DYNAMIC Thu Jun 18 21:54:43 UTC 2026 x86_64 GNU/Linux;native=wsl;physical=unavailable;smt=unavailable;thermal=unavailable;background=unavailable;pilot=8adee2c9057ac998e119c9ace51c874ad4496b15830f11f63c0f12b158fd564e`; rustc `rustc 1.97.0-nightly (9eb3be26b 2026-05-18) binary: rustc commit-hash: 9eb3be26b46eccea1de7448ea9cc3c1d20bb1a35 commit-date: 2026-05-18 host: x86_64-unknown-linux-gnu release: 1.97.0-nightly LLVM version: 22.1.4`. Evidence: `3eb8e047083615a41a84a089ec71bd15fad86364:12798164110868729919#0..3`. Limit: 180 s. Baseline: `tcp/readiness`.

| arm | payload | capacity | in-flight | memory | blocks | effect | interval |
|---|---:|---:|---:|---:|---:|---:|---:|
| evering/adaptive | 0 | 8 | 8 | 4198400 | 3 | 46.834092 | [42.483892, 49.868805] |
| evering/busy | 0 | 8 | 8 | 4198400 | 3 | 181.598331 | [170.875682, 183.426717] |
| evering/notified | 0 | 8 | 8 | 4198400 | 3 | 32.542539 | [32.298654, 37.407598] |
| evering/adaptive | 64 | 8 | 8 | 4198400 | 3 | 38.318867 | [35.312526, 40.578151] |
| evering/busy | 64 | 8 | 8 | 4198400 | 3 | 73.862998 | [73.151181, 74.326502] |
| evering/notified | 64 | 8 | 8 | 4198400 | 3 | 35.647651 | [34.307252, 38.556558] |
| evering/notified | 1024 | 1 | 1 | 4198400 | 3 | 5.446860 | [5.346567, 5.535386] |
| evering/notified | 1024 | 1 | 8 | 4198400 | 3 | 5.314536 | [4.144349, 5.325922] |
| evering/notified | 1024 | 1 | 64 | 4198400 | 3 | 4.978250 | [4.722502, 5.686724] |
| evering/notified | 1024 | 8 | 1 | 4210688 | 3 | 4.667180 | [4.466875, 5.055286] |
| evering/adaptive | 1024 | 8 | 8 | 4210688 | 3 | 18.166742 | [15.330602, 18.805495] |
| evering/busy | 1024 | 8 | 8 | 4210688 | 3 | 23.714871 | [22.104808, 26.217236] |
| evering/notified | 1024 | 8 | 8 | 4210688 | 3 | 17.405573 | [14.636112, 17.852650] |
| evering/notified | 1024 | 8 | 64 | 4210688 | 3 | 14.143019 | [14.062189, 15.884182] |
| evering/notified | 1024 | 256 | 1 | 4718592 | 3 | 5.582837 | [5.172693, 5.981229] |
| evering/notified | 1024 | 256 | 8 | 4718592 | 3 | 10.587621 | [10.055631, 13.358354] |
| evering/notified | 1024 | 256 | 64 | 4718592 | 3 | 3.058097 | [3.026271, 3.181640] |
| evering/adaptive | 16384 | 8 | 8 | 4456448 | 3 | 1.664847 | [1.618081, 1.703604] |
| evering/busy | 16384 | 8 | 8 | 4456448 | 3 | 2.618635 | [2.506219, 2.638095] |
| evering/notified | 16384 | 8 | 8 | 4456448 | 3 | 1.687608 | [1.658190, 1.919765] |
| evering/adaptive | 65536 | 8 | 8 | 5242880 | 3 | 1.198284 | [1.017979, 1.235198] |
| evering/busy | 65536 | 8 | 8 | 5242880 | 3 | 1.315089 | [1.188720, 1.374803] |
| evering/notified | 65536 | 8 | 8 | 5242880 | 3 | 1.151724 | [1.023869, 1.266778] |

## local-ipc-unix/v1 — screening

Screening is descriptive; it authorizes no performance decision.

Environment: `linux` / `x86_64-unknown-linux-gnu`; host `os=linux;arch=x86_64;logical=20;cpu=13th Gen Intel(R) Core(TM) i5-13500HX;affinity=0-19;page=4096;power=unavailable;kernel=Linux 6.18.33.2-microsoft-standard-WSL2 #1 SMP PREEMPT_DYNAMIC Thu Jun 18 21:54:43 UTC 2026 x86_64 GNU/Linux;native=wsl;physical=unavailable;smt=unavailable;thermal=unavailable;background=unavailable;pilot=acfcfc8e5ba8f4795f81947425f8942e2cc7b9bd3a329f0065cdc1877bcf6bbd`; rustc `rustc 1.97.0-nightly (9eb3be26b 2026-05-18) binary: rustc commit-hash: 9eb3be26b46eccea1de7448ea9cc3c1d20bb1a35 commit-date: 2026-05-18 host: x86_64-unknown-linux-gnu release: 1.97.0-nightly LLVM version: 22.1.4`. Evidence: `3eb8e047083615a41a84a089ec71bd15fad86364:11292933763636691026#0..3`. Limit: 90 s. Baseline: `uds/readiness`.

| arm | payload | capacity | in-flight | memory | blocks | effect | interval |
|---|---:|---:|---:|---:|---:|---:|---:|
| evering/adaptive | 0 | 8 | 8 | 4198400 | 3 | 3.063207 | [2.957071, 3.227880] |
| evering/adaptive | 64 | 8 | 8 | 4198400 | 3 | 3.081042 | [3.028182, 3.165040] |
| evering/adaptive | 1024 | 8 | 8 | 4210688 | 3 | 1.792174 | [1.541941, 2.024538] |
| evering/adaptive | 16384 | 8 | 8 | 4456448 | 3 | 1.090224 | [1.085195, 1.096696] |
| evering/adaptive | 65536 | 8 | 8 | 5242880 | 3 | 1.193623 | [1.093421, 1.210435] |

