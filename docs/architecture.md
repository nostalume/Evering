# Evering Architecture

## Purpose

Evering is a low-level substrate for bounded communication through a shared
memory region. Its core model does not depend on an operating system: shared
layouts, allocation authority, queues, ownership transfer, and recovery are
expressed entirely in mapped bytes and atomics. Optional adapters establish a
mapping, exchange native resources, notify a peer, or connect the progress
model to an asynchronous runtime.

The design favors concrete types, constant geometry, monomorphization, and
inlining where they change shared interpretation or the hot path. Construction
policy is erased once admitted. This keeps address preferences, native handles,
and runtime choices out of persistent types and avoids the long generic chains
that previously coupled allocator, registry, queue, and driver policy.

Evering is not a scheduler, process manager, discovery service, or request
dispatcher. Application protocols own correlation, retry, idempotency, and
deadlines. Evering supplies the shared transport and the evidence needed to
move or recover its storage authority.

## System model

A session is one admitted view of a mapped region. The region contains a
Directory, an isolated general heap, recoverable Pools, and Directory-owned
resources such as duplex Channels. Each persistent layout records and validates
its own schema and immutable information. There is no region-wide manifest that
must enumerate every possible layout or message type.

The Directory gives a current lifetime to independently typed layouts. The
general heap supplies variable-sized storage for Directory construction and
explicit shared boxes. A Pool supplies recoverable transfer storage. A Channel
contains two bounded queues and derives direction from an admitted role. The
session itself remains protocol-neutral; a protocol type appears when a Channel
is created or a typed Port is admitted.

Shared bytes are authoritative for layout state, participant generations,
allocation lifecycle, queue progress, and endpoint gates. Process-local owners
are authoritative for mapping lifetime, native resources, task registration,
and typed borrows. A pointer is always derived from an admitted mapping and
validated metadata; it never serves as transferable ownership evidence.

| Concern | Authority | Permitted effect |
| --- | --- | --- |
| Local mapped extent | Linear mapping owner | Transfer the view at root admission and release it exactly once |
| Persistent layout | Its recorded header | Initialize or admit one schema, information value, extent, and body |
| Layout lifetime | Generational Directory entry | Create, open, close, recover, quarantine, or remove the current occupant |
| General allocation | Synchronized heap mutation | Allocate explicit boxes and construction storage within its failure domain |
| Transfer allocation | Pool lifecycle word | Reserve, detach, adopt, release, or reclaim one generation of one slot |
| Transport position | Queue sequence and cursors | Publish, claim, cancel, recycle, and establish terminal drain |
| Channel direction | Admitted role generation | Derive the only legal transmit and receive sides |
| Progress advice | Borrowed local Signals | Notify after commit, withhold one notification per batch of commits, or wait before retrying authoritative shared state |
| Retry patience | A policy the retrying caller holds | Spend local spins, wait for the peer, or stop the operation |
| Process death | Exact supervisor or platform evidence | Authorize recovery only for the named participant generation |

The principal data flow is mapping admission, layout admission, Pool
reservation, payload initialization, Token publication, queue claim, Pool
admission, application consumption, and storage release. Optional process
handoff precedes mapping admission; optional notification follows publication
or recycling. Each arrow is a move or checked admission. No step copies local
authority into the shared representation.

## Representation and admission

The mapping boundary accepts one linear local mapping with an extent, access
mode, and exactly-once release operation. Successful root admission transfers
that owner to the region. Every failure releases it. Platform sources may use
files, anonymous sections, reserved memory, or another mechanism, but their
types and errors do not survive admission.

The root has a caller-chosen region identity. Creation may initialize an empty
root. Expected-identity and discovery modes only attach, so a peer cannot turn
missing shared state into a new region by accident. Joining a region needs
writable access, because attachment updates participant state. Reading one does
not: `Observer::discover` maps the same extent under the same access
declaration, reports which region the published root claims, and answers what a
published record says it is. It holds no member slot and takes no part in the
roster, so what it can name is the shape of what is published rather than a
position in the exchange, and it is not a consumer of anybody's queue.

Every layout begins with a fixed record followed by layout-owned information
and body bytes. The record binds state, magic, recursive schema, region,
position, complete extent, and alignment. Admission validates that fixed record
before interpreting typed information or projecting the body. Initialization
claims empty storage atomically, initializes through an uninitialized
destination, then publishes with release ordering. A failed claimed
initialization becomes corrupted instead of appearing absent.

Schema composition includes every type, constant, and nested revision that
changes shared interpretation. A nested incompatible change therefore changes
the enclosing identity. Compatibility is behavioral across native word widths:
each architecture may use its natural representation, but both obey the same
state transitions and reject values that cannot be represented locally. Within
that promise, a new or touched shared word is written fixed-width little-endian
with an explicit atomic-width class, the shape `Id<L>::to_bytes` in
[`src/dir.rs`](../src/dir.rs) already sets, so a field stops inheriting its width from the host the day it is
written; the natural-width fields that predate this are left as they are, because a
peer of a different width never reads them — it is refused at admission before the
first natural-width word. The project does not promise
a byte-identical Rust ABI across builds or targets.

## Storage and ownership

General allocation and transferable allocation have different failure domains.
The general heap is a synchronized Talc domain. It remains useful for compact,
variable-size allocation, but its multi-field mutation cannot be treated as the
sole crash-recoverable ownership ledger. It is therefore isolated to Directory
layouts and explicit shared boxes. Poisoning may reject later general-heap
operations but cannot revoke an admitted Pool or Channel.

A Pool has immutable creation-time geometry and one authoritative lifecycle
word per slot. Geometry may use several size classes and upward spill; that is
an implementation policy, not part of the public capability type. A request
that no class can represent fails explicitly. It never falls back silently to
the general heap. Large logical objects may be streamed through several Blocks
without changing the bound of one Block.

Reservation gives a process-local Block exclusive payload authority. Encoding
or typed transfer creates a private Token and retains local allocation authority
until queue publication commits. Publication detaches the allocation from the
sender. Admission checks Pool identity, slot, generation, metadata, runtime
type, extent, and alignment before reconstructing a pointer and moving
authority to the receiver. Release returns the slot only from that receiver
authority.

Runtime-classified messages use a zero-sized Encoded protocol marker. Their
Token type identifier is derived from the supplied protocol schema and body
schema. The receiver can inspect that identifier and admit the expected body in
one operation. A mismatch preserves the same claim; success returns the body,
not a redundant marker or second validation record.

## Transport and channels

A queue transports a fixed shared representation. It does not allocate,
interpret application objects, reconstruct pointers, or own notification.
Sequence stamps authorize slot reuse, while header cursors and gates describe
producer and consumer progress. A failed send returns the exact input.

A duplex Channel owns two queues. Admitting a Port assigns one unique role, and
splitting derives the legal outbound and inbound directions from that role.
There is no user-selected left/right choice that can invert behavior. Concrete
Tx and Rx values hold only their admitted direction and local mapping lifetime.

Receiving produces a claim-bound Received authority. Dropping it intentionally
does not recycle the queue slot because detached Pool storage may still depend
on that claim. The receiver must admit the storage or explicitly discard it.
Both operations resolve Pool authority before recycling the slot. Whole-Channel
removal applies the same order to queued Tokens and resolves each private Pool
identity through the Directory; callers cannot provide the wrong allocator.

Closing serializes with sender admission. A receiver observes terminal closure
only after senders admitted in the open epoch have left and the queue is empty.
Removal terminalizes both directions, drains governed storage, and then returns
the Directory entry to reuse. Uncertain evidence retains or quarantines the
resource rather than guessing that it is empty.

## Progress and process adapters

Notification is advisory. Queue and gate state remain authoritative. Signals
borrows a notification capability and, when asynchronous progress is required,
a wait capability. The null Signals form supports nonblocking and post-commit
operations but cannot wait; the type system prevents an async claim without a
real wait source.

Publication, recycling, and close update shared truth before notifying the
peer, unless a batch withholds that notification. `Signals::defer` hands out one
guard, every batched publish or recycle must hold it mutably, and it delivers
the withheld notification when the batch asks or, if the batch never asks, when
the guard leaves scope. A withheld notification therefore has a deadline, and a
batch that committed nothing rings nothing. Close is deliberately not
batchable: a peer parked on a terminal state must be told before the batch ends.
Notification failure therefore cannot revoke committed work or return
moved input. A committed result exposes its value separately from notification
health. Waiting registers process-local interest, observes and consumes sticky
readiness, then immediately retries shared truth — except for the one readiness
that cannot be consumed, a child's death, which keeps reporting itself until the
owner that reaps it takes the status. Wakers, callbacks, reference counts, and
native handles never enter the shared protocol.

A latch is defined by what it must guarantee, not by its primitive. Every
adapter is sticky, may coalesce without counting, and consumes nothing when a
wait is cancelled. It promises nothing about how many waiters one notification
releases or about which change caused it, and the model survives both answers
because one primitive is paired with one condition — one ring, one direction —
so a wake that releases only its head waiter, as a kernel notification does,
still leads to re-examined shared truth. Whether a latch is empty after a park is
not observable through a wait, so an adapter's further properties are proved
against the primitive itself in its own module. An extension that reports a cause
would be admissible only where a provider can both carry it and be checked
carrying it, and only to skip work a re-examination could have found. The clauses
a new provider is checked against are recorded in
[`tests/notification_latch/latch_clauses.rs`](../tests/notification_latch/latch_clauses.rs).

A park can also be asked of a whole set of latches, which reports the one that
answered and consumes that one alone. It multiplexes latches rather than causes, so
one primitive is still paired with one condition, and a caller holding N peers
parks once across them instead of keeping a task per peer.

How long that retry continues is a policy the caller holds, not a constant in
the queue. Each failed attempt reports its reason, local contention or a
peer-bound condition, and the policy answers with a spin, a wait, or a stop.
The library's ladder bounds the local spins of one operation and then waits on
the peer's readiness, restoring that bound after a wait. Keeping the two
reasons apart is what makes a policy safe to inject: waiting for the peer on
local contention can sleep through the progress that would have resolved it.
A policy that always waits instead runs one operation to commit, and an agent
holding both sides of a duplex channel stops reading while it does that. If the
peer is written the same way, each waits on the ring the other would free by
reading, and nothing inside either loop can notice, so an agent with both roles
bounds its budget or attempts a read between publishes.
[`tests/publish_and_kick/ring_limit.rs`](../tests/publish_and_kick/ring_limit.rs) demonstrates both shapes against one
geometry.

Native latches may coalesce signals or wake spuriously. Concurrent waiters are
allowed, cancellation unregisters only local interest, and a later notification
remains observable. An async sender waits for a reservation before accepting
its payload; after that typestate transition, staging and publication contain
no suspension point.

Process adapters exchange an opaque bounded bootstrap value followed by an
ordered mapping, wait event, and notification ring. The framing magic owns its
version. Receipt is all-or-nothing: count or framing failure closes every
received resource. Supervision retains one exact child identity and terminal
status; a timeout or task cancellation is not proof that a process can no
longer access shared memory.

The same rendezvous carries the admitted process's report of the member it
became, framed under its own magic and credited by the host rather than by the
sender. A promise, that report, and a reaped exit of the same process are what
authorize a death: a report that disagrees with the promise, or an exit of a
different process, authorizes nothing. A report is read under a deadline the
caller names, because a datagram rendezvous answers a peer's departure with no
signal at all, so the reap and not the socket is the death evidence.

## Recovery model

Recovery begins only with authoritative participant-death evidence. Two
entrances carry it: a caller's own assumption about an identity it can name,
which is unsafe by construction, and a receipt the host credited to a process
that was then reaped, which is not. Participant slots carry generations so an
observation about an earlier process cannot be applied to a replacement. Directory operations persist their transaction owner
and phase. Recovery can finish or roll back from those states without inferring
intent from payload bytes.

Pool recovery scans lifecycle words for the exact dead owner and reclaims only
matching generations. Queue recovery preserves the source owner of a reserved,
published, or claimed transition. If a recovery participant dies, a replacement
continues from that persisted source rather than substituting its own identity.
Storage evidence is resolved before the queue advances, preventing a reused
slot from overwriting the Token needed for reclamation.

The general heap has a stricter boundary. A dead owner at a mutation point that
has durable Directory evidence can be resolved by that enclosing transaction.
An ambiguous heap mutation is poisoned and its dependent layout retained or
quarantined. This fail-stop outcome trades availability for avoiding fabricated
ownership. Large data that needs crash-tolerant transfer must use Pool-backed
Blocks or an application protocol over them, not an implicit heap allocation.

## Invariants and limits

The essential invariants are:

- shared state contains no process-relative pointer, callback, native handle,
  task waker, or local reference count;
- each layout validates its own schema, immutable information, position,
  extent, and region identity before typed projection;
- each moved Pool allocation has exactly one reclamation authority through
  reserve, publish, claim, admit or discard, release, cancellation, closure,
  and proven peer death;
- failed transfer admission preserves the same Received authority, and
  notification failure after commit never becomes a pre-commit error;
- Directory and Pool identifiers are accepted only for their current
  generations, which retire instead of wrapping;
- terminal queue closure excludes later publication from the closed epoch;
- process-local mapping and resource owners release exactly once.

Current evidence covers model-level interleavings, typed admission failures,
Directory and Pool generation reuse, queue contention and closure, malformed
resource exchange, different-base process mappings, asynchronous notification,
and bounded worker recovery. Two arms are exercised natively,
`x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`; the arrival receipt is
executed only on the first, because its tests name Unix, so that leg of the
handover seam is compiled but untested on Windows. The practical
indexer demonstrates public construction, process handoff, runtime waiting,
typed payload admission, close, and removal. Each example is executed as a
process by ordinary CI on both arms and asserts on the lines its legs print, so
what it demonstrates is gate-run rather than only compiled.

The crate defaults to standard-library support; its core also compiles with
default features disabled. Native mapping, notification, process exchange,
Tokio waiting, tracing, evidence capture, comparison transports, and plotting
are additive capabilities. Bare-metal or restricted systems may supply their
own mapping and polling environment without a fake signal implementation.

Evering remains experimental. It does not promise cross-build ABI stability,
membership that cannot write, automatic replay of application work, universal
allocator recovery, or performance superiority outside a registered and
matched study condition. Read-only observation is offered, and it is offered as
a standpoint outside the roster rather than as a kind of membership. These
limits are protocol boundaries, not hidden fallback behavior.

## Where each claim is owned

The published pages divide the same material by question, and the division is a rule
rather than a habit. [AGENTS.md](AGENTS.md) states it once: which page owns which kind of
statement, which page wins when two of them disagree, and that a change moves the pages it
affects in one commit. A sentence about a verdict therefore belongs on the verdict page even
when the model is what motivated it, and a sentence about the model belongs here even when a
test is what proves it.
