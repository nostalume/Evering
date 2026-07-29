# Evering Architecture

## Purpose

Evering is a low-level, operating-system-independent model for bounded
communication through a shared memory region. It combines region layout,
relocatable allocation, typed ownership transfer, reusable duplex queues, and
optional asynchronous correlation.

The performance model favors static composition for persistent layouts and hot
operations: message protocol, queue geometry, endpoint role, and typed layout
remain concrete so the compiler can monomorphize and inline the active path.
Platform mapping policy ends at construction and does not propagate through
admitted layout or session types.

Evering is a communication framework, not a complete asynchronous runtime.
Scheduling, peer discovery, process creation, and liveness detection remain
outside the core. Optional native latches and runtime waiting adapt shared
progress without becoming shared protocol state.

## System model

The central object is a session over one mapped region. A session owns two
cooperating facilities:

- an allocator that converts allocations into region-relative metadata and can
  reconstruct local pointers from that metadata;
- a bounded registry that creates, publishes, acquires, and eventually recycles
  communication resources.

A registered resource is normally a duplex pair of bounded queues. Each process
maps the region locally, obtains a view of the same registry entry, chooses a
left or right role, and derives opposite sender and receiver directions.

The shared region is the source of truth for allocator metadata, registry
entries, queue state, and message storage. Process-local values provide views,
mapping lifetime, polling state, task wakers, and endpoint role.

## Conceptual boundaries

### Mapping

The mapping boundary turns a platform or application-owned source into one
linear local mapping with explicit extent, access, and exactly-once release
authority. A successful root admission transfers that mapping to the region;
every failure releases it before returning. Region layout then places typed
headers in order and either initializes them or attaches to an existing
compatible header.

Platform address preferences, mapping flags, handles, and source errors exist
only at this boundary. Admitted layouts, sessions, allocators, registries, and
queues retain no backend type, allocation, trait object, or platform policy.
Custom bare-metal and RTOS sources may supply mapped reserved memory through an
explicit unsafe ownership contract.

### Region layout and initialization

The mapping root carries a caller-supplied region identity. Creation may
initialize an empty root; expected-identity and discovery modes are attach-only.
This keeps identity authority outside the operating-system-independent core and
prevents an attaching process from silently constructing absent shared state.

Every persistent layout independently records a fixed admission prefix and
layout-owned typed information. The prefix identifies its state, magic,
recursive schema key, region, position, and the size and alignment of the
complete typed header, information value, and body. The typed information
records facts owned by that layout, such as registry capacity and resource
schema, allocator strategy, or usable geometry. Composite schema keys consume
both the identifier and revision of each nested layout, so changing a nested
revision changes every enclosing protocol identity.

Initialization claims empty storage through an atomic state transition,
initializes through a raw destination, and publishes completion with release
ordering. Attachment first validates the fixed prefix and only then interprets
typed information or the body. Rejection never falls back to initialization,
and a failed claimed initialization publishes a corrupted state.

Layout remains positional and compile-time typed, while composition remains
open-ended. A reservation exclusively borrows its composition cursor and is
committed only through that cursor; successful admission advances it. A failed
commit poisons the local cursor so callers cannot continue from an ambiguous
position. Participants must agree on the ordered prefix they access, but a
prefix-only participant does not need to know or validate independent suffix
layouts.

Admission currently requires a writable mapping. Even an attaching root
updates shared participation state, so read-only mapping is rejected before
typed admission. Read-only observation would require a different lifetime
model.

### Allocation

Evering uses a synchronized variable-size talc allocator for shared-memory
payloads and queue storage. Its internal links and exported allocation metadata
are region-relative, allowing the same allocation to be recalled from another
mapping base.

Allocation returns portable metadata and records the identity of the allocator
layout that issued it. Reconstruction asks the receiving process's allocator
view to admit that identity, metadata, expected extent, and alignment before a
typed pointer is formed. Deallocation must return the exact allocation to the
same shared allocator domain.

A session exposes a short-lived heap view over that allocator. The heap owns no
shared lifetime and is not itself transferable; it centralizes typed move,
slice-copy, and reconstruction operations so callers do not repeatedly thread
allocator context through message-management code.

### Registry

The registry owns reusable shared resources. A generational identifier selects
an entry and distinguishes its current lifetime from older occupants of the
same slot.

Preparation claims a free entry and constructs its resource using the session
allocator. Acquisition projects that resource into a process-local view while
retaining an entry guard. The last guard finalizes the resource. A resource is
reusable only when finalization proves it empty; otherwise its entry is
quarantined and cannot be acquired, cleared, or returned to the free list.
Exhausted generations are retired rather than wrapped.

Session orchestration allocates a queue, transfers the resulting value into the
registry, and releases it if insertion fails. The registry owns the installed
queue lifetime; endpoints and messages do not.

### Messages and tokens

A message declares a deterministic type tag and transfer semantics. Move
semantics allocate the value in shared memory and replace ownership with a
token. Declaring a message or envelope is an unsafe portability contract:
implementations promise a stable initialized representation without
process-local pointers, callbacks, runtime handles, or local allocator
ownership.

A token contains:

- the identity of the allocator layout that issued it;
- allocator-specific relative metadata;
- sized or slice pointer metadata;
- a deterministic message type identifier.

The token does not contain a process-local allocator handle. Reconstruction
requires the receiver to supply its own view of the identified shared allocator.
Type identification and reconstruction are fallible and return the still-owned
token and allocator on rejection.

An envelope is orthogonal metadata carried beside the token. The implemented
request envelope adds a generational operation identifier for response
correlation.

### Channels

A shared queue stores message tokens, not application objects or process-local
capabilities. Queue slots use atomic sequence stamps, and queue headers track
bounded producer/consumer progress and disconnection.

A duplex resource contains two queues. Role-specific splitting makes one queue
outbound and the other inbound for each participant. Closing an endpoint changes
shared connection state. Close serializes with sender admission, and a receiver
reports terminal disconnection only after admitted senders have left and a
final empty observation. A rejected send returns the exact input. Finalization
reopens an empty resource for reuse and quarantines a nonempty one.

Local channels and shared-token channels implement the same sender, receiver,
and queue-channel concepts but differ in storage ownership.

### Asynchronous correlation

The optional driver layer is process-local. A bounded cache pool assigns a
generational operation identifier before submission, stores completion state
and a task waker locally, and places only the identifier in the shared envelope.

The remote participant returns the envelope unchanged. A local completion pump
receives the response, resolves the identifier, stores the result, and wakes the
waiting future. Submission failures return the original request value.
Completion claims a cache state before writing; duplicate, stale, or retired
completion returns the response payload to the caller. Cancellation retires a
slot when its generation can no longer advance.

This correlation remains process-local and distinct from progress notification.

### Progress notification

Notification is advisory; queue and gate state remain authoritative. An async
endpoint combines an unchanged nonblocking endpoint with one local ring owner
and one local wait owner. A failed queue attempt waits for sticky readiness,
clears it, and retries the shared operation. No reservation, claim, shared
borrow, callback, or task waker crosses the wait.

Successful publication or capacity release rings the peer after shared state
has committed. A ring failure therefore cannot roll back the operation or
return already-moved input. The result reports the committed value separately
from notification health. Close follows the same order: publish the shared gate
transition, then advise the peer to recheck it.

Linux uses an event counter, other Unix targets use a nonblocking socket latch,
and Windows uses a manual-reset event. Runtime registration and cancellation
are process-local. Readiness may coalesce or be spurious, and clearing may
consume a concurrent ring; the mandatory retry observes shared truth
immediately afterward, while a later ring remains latched. No notification
word, handle, or extra atomic operation is added to a duplex layout.

## End-to-end flow

1. A creator maps a backing region with a supplied identity; peers map it with
   an expected identity or explicitly discover the published identity.
2. One participant prepares a duplex resource and communicates its generational
   identifier out of band.
3. Each participant acquires the resource and selects its opposite endpoint
   role.
4. The sender allocates a message in shared memory and converts it into a token.
5. If correlated completion is used, a local cache entry supplies a numeric
   operation identifier that is attached to the envelope.
6. The outbound queue moves the token into shared storage and, when configured,
   rings the peer only after publication commits.
7. The receiver dequeues the token, verifies its type identifier and allocator
   layout identity, and admits its extent and alignment through the local
   allocator view before reconstruction.
8. The receiver consumes or transforms the message, creates a response token,
   and returns the correlation envelope through the opposite queue.
9. A waiting endpoint rechecks shared state after each advisory wake. The
   originating process resolves the operation identifier, wakes the correlated
   task, recalls the response, and eventually deallocates it.
10. Endpoint guards and mapping handles release process-local participation;
    registry finalization governs resource reuse.

## Ownership and authority

| Concern | Owner | Authority |
| --- | --- | --- |
| Local mapping | Linear mapping owner and source | Establish one local view, transfer it at root admission, and release it exactly once |
| Region identity | Mapping root and caller admission policy | Initialize or validate one immutable shared-memory domain identifier |
| Persistent layout admission | Each typed header | Record and validate its own schema, position, representation, and immutable information |
| Layout order | Composition cursor | Place and attach independent typed layouts without a session-wide manifest |
| Shared allocation | Selected allocator layout | Allocate, admit token provenance and extent, reconstruct, and deallocate within one allocator domain |
| Resource lifetime | Registry entry | Construct, project, finalize, recycle proven-empty resources, and quarantine uncertain ownership |
| Payload ownership | Message token plus protocol state | Move one allocation between participants |
| Queue direction | Endpoint role | Select the legal outbound and inbound queue |
| Request correlation | Shared generational ID and local cache | Carry portable identity; retain waker/result locally |
| Progress notification | Local ring and wait owners | Advise a peer after commit; register task readiness and clear the local latch |
| Scheduling and polling | Application/runtime | Drive retries, completion pumping, timeout, and cancellation |

## Required invariants

- Shared state contains no raw pointer whose meaning depends on one process's
  mapping base.
- Relative allocation metadata is interpreted only by a compatible view of the
  allocator domain that created it.
- Typed reconstruction occurs only after allocator identity, extent, layout,
  and alignment admission.
- A token's type identifier, pointer metadata, allocation metadata, and actual
  allocation layout agree.
- Every moved allocation has exactly one active reclamation authority, including
  queue-full, disconnect, cancellation, and peer-death paths.
- Terminal queue disconnection implies that no sender admitted in that open
  epoch can publish later.
- Failed send, submit, type admission, reconstruction, and completion return
  the rejected ownership rather than discarding it.
- A registry identifier is accepted only while both its index and generation
  identify the current entry lifetime.
- Queue capacity is nonzero and all participants agree on queue representation
  and dynamic geometry.
- A persistent header is used only after successful initialization or compatible
  attachment.
- Persistent header, information, and body extents match before typed
  information or body projection.
- An attaching mapping cannot initialize an absent root or child layout.
- Fixed admission fields are validated before layout-owned information or body
  bytes are interpreted as the expected type.
- Every layout belongs to the admitted region identity and its recorded
  region-relative position.
- A layout schema composes every generic or const parameter that changes its
  shared interpretation.
- Mapping sources must provide exclusively owned valid bytes, accurate extent
  and access, and an exactly-once release operation; platform handles, flags,
  and errors do not survive admission.
- Process-local wakers, reference counts, mapping handles, and allocator views
  never enter shared queue entries.
- Notification failure after commit never fabricates pre-commit failure or
  returns moved ownership.
- Cancellation removes only process-local wait registration; shared queue,
  gate, and payload state remain unchanged.

## Current evidence and limits

The repository has unit tests for mapping, state-driven layout admission,
allocator provenance rejection, registry reuse and quarantine, invalid
identifiers, local queues, lossless shared-token queue rejection, correlation
cache ownership, static transfer bounds, initialization failure publication,
representation-extent rejection, cursor poisoning, and concurrent thread
access. Unix integration evidence covers process-isolated root identity
rejection, process-isolated attach-only behavior, prefix-only attachment,
registry schema mismatch, recursive child-revision mismatch, process-isolated
talc geometry rejection, write-required admission, and a different-base
independent-process message round trip. The benchmark exercises the complete
message/token/channel flow over a shared mapping.

This evidence supports relocation, compositional same-build layout admission,
allocator-layout provenance admission, direct queue transport across two Unix
processes, exhaustive notification ordering at the abstract latch boundary,
and real-process asynchronous notification on Linux and Windows. It does not
prove compatibility across Rust builds or target architectures, read-only
participation, or public mapping-and-notification bootstrap exchange.

Unix mapping and notification backends are implemented. Windows notification
is implemented, while a Windows mapping backend is not.

The crate defaults to standard-library support without selecting a platform
adapter. Its `no_std + alloc` configuration compiles on the pinned nightly when
default features are disabled. Unix mapping and process evidence additionally
selects the mapping capability.

Type tags are deterministic within the declared scheme, but the complete shared
ABI is still Rust-layout dependent. Compatibility is therefore limited to peers
that agree on build, architecture, protocol types, allocator, capacities, and
session composition.

Token ownership remains explicit and fallible. Quarantine prevents uncertain
queue ownership from being silently recycled, but recovery or reclamation after
peer death remains unresolved.
