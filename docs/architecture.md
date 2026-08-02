# Evering Architecture

## Purpose

Evering is a low-level, operating-system-independent model for bounded
communication through a shared memory region. It combines region layout,
relocatable allocation, typed ownership transfer, reusable duplex queues, and
optional advisory progress notification.

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

The central object is a protocol-neutral session over one mapped region. A
session owns three cooperating facilities:

- a Directory that identifies independently typed shared layouts and governs
  their creation, admission, recovery, and removal;
- one general heap for explicit variable-size values and Directory storage;
- recoverable Pools whose lifecycle words, rather than allocator metadata,
  authorize transferable Blocks.

A Channel is a Directory-owned duplex pair of bounded queues. Channel protocol
type enters when that Channel is created or its typed Port is admitted; it does
not parameterize the Session. Each process derives opposite sender and receiver
directions from its admitted role without a runtime side choice.

The shared region is the source of truth for Directory entries, GeneralHeap and
Pool metadata, queue state, and transferable storage. Process-local values own
mapping lifetime, typed admission, polling state, task wakers, and endpoint
role.

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
records facts owned by that layout, such as queue capacity and resource schema,
Pool classes, or usable geometry. Composite schema keys consume
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

### Storage

Evering separates general allocation from recoverable transfer storage. The
GeneralHeap is a synchronized variable-size Talc domain used for explicit PBox
values and for allocating Directory-owned layouts. Talc remains isolated: its
poison state may reject new layouts or PBox values, but it cannot revoke an
already admitted Channel or Pool and is never an implicit Pool fallback.

A Pool divides one immutable extent into recorded geometry and gives each slot
one authoritative lifecycle word. Reservation grants local Block authority;
publication changes that authority to detached transfer ownership; claim-bound
admission moves it to the receiver. Pointer reconstruction is derived only
after Pool identity, generation, span, runtime type, extent, and alignment agree.

Pool geometry may use several fixed size classes and upward spill, but geometry
is an implementation policy rather than the ownership contract. A Pool never
silently allocates from GeneralHeap. Large logical objects may use multiple
Blocks without changing the bounded size of one capability.

### Directory

The Directory owns reusable shared layouts. A generational identifier selects
an entry and distinguishes its current lifetime from older occupants of the
same slot. Every layout records and validates its own schema and information;
there is no session-wide manifest or runtime protocol classification.

Creation claims a free entry, allocates its layout from GeneralHeap, initializes
it, and returns the admitted owner directly. Opening by identifier admits the
recorded layout without reconstructing creation options. Owned Pool and Channel
values keep their mappings alive independently of a Session borrow. Exhausted
generations are retired rather than wrapped.

Whole-Channel removal is owned by Session. It terminalizes both directions,
resolves each queued Token's private Pool identity through the Directory,
releases storage, and only then recycles the Queue slot. A caller cannot supply
a Pool and therefore cannot select the wrong storage authority. Unknown
evidence returns the same Channel for inspection or retry.

### Messages and tokens

A message declares a deterministic type tag and transfer semantics. Move
semantics allocate the value in shared memory and replace ownership with a
token. Declaring a message or envelope is an unsafe portability contract:
implementations promise a stable initialized representation without
process-local pointers, callbacks, runtime handles, or local allocator
ownership.

A private token contains:

- the identity of the Pool that issued it;
- its class, slot, and allocation generation;
- sized or slice pointer metadata;
- a deterministic message type identifier.

The token is not a pointer or public reconstruction capability. A receiver must
provide an admitted view of the identified Pool. Type and lifecycle rejection
return the same claim-bound Received authority; dropping it deliberately does
not recycle a slot that still governs detached storage.

An envelope is orthogonal metadata carried beside the token. The implemented
request envelope adds a generational operation identifier for response
correlation.

### Channels

A shared queue stores fixed transfer records, not application objects or
process-local capabilities. Their storage locators are private and cannot admit
a pointer. Queue slots use atomic sequence stamps, and queue headers track
bounded producer/consumer progress and disconnection.

A duplex resource contains two queues. Role-specific splitting makes one queue
outbound and the other inbound for each participant. Closing an endpoint changes
shared connection state. Close serializes with sender admission, and a receiver
reports terminal disconnection only after admitted senders have left and a
final empty observation. A rejected send returns the exact input. Finalization
reopens an empty resource for reuse and quarantines a nonempty one.

Concrete transmit and receive endpoints own only their admitted direction and
process-local mapping lifetime. The queue kernel remains independent of message
storage and notification policy. Receiving yields a claim-bound value that can
only adopt storage through the matching Pool or explicitly discard it. Either
operation moves Pool authority before recycling the queue slot; there is no
public locator-only receive path.

### Request correlation

The substrate does not own request identifiers, pending calls, response
dispatch, or task wakers. Those policies belong to a higher protocol because
they require application-specific cancellation, duplicate, late-response, and
shutdown semantics. Shared channels transport the protocol's fixed
representation without interpreting correlation fields.

### Progress notification

Notification is advisory; queue and gate state remain authoritative. A
signaled operation borrows an unchanged nonblocking endpoint, one local ring
owner, and one local wait owner. Waiting fuses registration, readiness
observation, and latch consumption before immediately retrying shared truth.
No endpoint, claim, shared borrow, callback, or task waker crosses that wait.

Successful publication or capacity release rings the peer after shared state
has committed. A ring failure therefore cannot roll back the operation or
return already-moved input. The result reports the committed value separately
from notification health. Close follows the same order: publish the shared gate
transition, then advise the peer to recheck it.

An asynchronous send waits for a Queue reservation before taking its payload.
The resulting permit moves, stages, publishes, and notifies without another
suspension. Cancelling the wait leaves the payload in caller scope; cancelling
the permit publishes its rollback before advising the peer. Receive notification
is delayed until adoption or discard recycles the claimed slot. Deadlines and
bounded spinning remain runtime policy around this adapter.

Linux uses an event counter, other Unix targets use a nonblocking socket latch,
and Windows uses a manual-reset event. Runtime registration and cancellation
are process-local. Multiple waits may coexist; readiness may coalesce or be
spurious, and concurrent consumption is idempotent. The mandatory retry observes
shared truth immediately afterward, while a later ring remains latched. No
notification word, handle, or extra atomic operation is added to a duplex layout.

## End-to-end flow

1. A creator maps a backing region with a supplied identity; peers map it with
   an expected identity or explicitly discover the published identity.
2. One participant creates a typed Channel and communicates its typed Port out
   of band.
3. The peer admits the Port, and each Channel derives its opposite directions.
4. The sender reserves a Block from an explicit Pool and turns it into a
   transfer capability.
5. An application protocol may attach a portable operation identifier without
   placing its local completion state in shared memory.
6. The outbound queue moves the token into shared storage and, when configured,
   rings the peer only after publication commits.
7. The receiver claims the transfer, verifies its runtime type and Pool
   lifecycle evidence, and admits its extent and alignment before pointer
   reconstruction.
8. The receiver consumes or transforms the message and may return a response
   through the opposite queue according to its application protocol.
9. A waiting endpoint rechecks shared state after each advisory wake. Any
   request correlation and task wakeup remains process-local application state.
10. Endpoint guards and mapping handles release process-local participation;
    Session removal drains Pool storage before Directory reuse.

## Ownership and authority

| Concern | Owner | Authority |
| --- | --- | --- |
| Local mapping | Linear mapping owner and source | Establish one local view, transfer it at root admission, and release it exactly once |
| Region identity | Mapping root and caller admission policy | Initialize or validate one immutable shared-memory domain identifier |
| Persistent layout admission | Each typed header | Record and validate its own schema, position, representation, and immutable information |
| Layout order | Composition cursor | Place and attach independent typed layouts without a session-wide manifest |
| Shared allocation | Pool | Reserve fixed storage, move participant ownership, admit provenance and extent, reconstruct a borrow, and release it |
| General allocation | General heap | Manage explicit PBox values outside the recoverable transfer path |
| Resource lifetime | Directory entry | Construct, admit, close, remove proven-empty resources, and retain uncertain ownership |
| Payload ownership | Pool lifecycle plus a claim-bound transfer record | Move one allocation between participants without exposing a pointer capability |
| Queue direction | Endpoint role | Select the legal outbound and inbound queue |
| Request correlation | Application protocol | Define identifiers, duplicate/late-response policy, cancellation, and local task state |
| Progress notification | Borrowed local ring and fused wait owners | Advise a peer after commit; observe and consume sticky readiness before retry |
| Scheduling and polling | Application/runtime | Drive retries, completion pumping, timeout, and cancellation |

## Required invariants

- Shared state contains no raw pointer whose meaning depends on one process's
  mapping base.
- Relative Block metadata is interpreted only by the Pool that created it.
- Typed reconstruction occurs only after allocator identity, extent, layout,
  and alignment admission.
- A token's type identifier, pointer metadata, allocation metadata, and actual
  allocation layout agree.
- Every moved allocation has exactly one active reclamation authority, including
  queue-full, disconnect, cancellation, and peer-death paths.
- Terminal queue disconnection implies that no sender admitted in that open
  epoch can publish later.
- Failed send returns its exact input. Failed transfer admission returns the
  same claim-bound authority for retry or explicit discard; implicit drop never
  recycles detached Pool storage.
- A Directory identifier is accepted only while both its index and generation
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
Pool provenance rejection, mixed-Pool Channel removal, Directory reuse and retained uncertain ownership,
invalid identifiers, local queues, claim-bound transfer rejection, static
transfer bounds, initialization failure publication,
representation-extent rejection, cursor poisoning, and concurrent thread
access. Unix integration evidence covers process-isolated root identity
rejection, process-isolated attach-only behavior, prefix-only attachment,
Directory schema mismatch, recursive child-revision mismatch, process-isolated
talc geometry rejection, write-required admission, and a different-base
independent-process message round trip. The benchmark exercises the complete
message/Pool/channel flow over a shared mapping.

This evidence supports relocation, compositional same-build layout admission,
Pool provenance admission, direct queue transport across two Unix
processes, exhaustive notification ordering at the abstract latch boundary,
and real-process asynchronous notification on Linux and Windows. It does not
prove compatibility across Rust builds or target architectures, read-only
participation, or public mapping-and-notification bootstrap exchange.

Unix and Windows mapping, process-resource exchange, and notification adapters
are implemented behind optional capabilities.

The crate defaults to standard-library support without selecting a platform
adapter. Its `no_std + alloc` configuration compiles on the pinned nightly when
default features are disabled. Unix mapping and process evidence additionally
selects the mapping capability.

Type tags are deterministic within the declared scheme, but the complete shared
ABI is still Rust-layout dependent. Compatibility is therefore limited to peers
that agree on build, architecture, protocol types, allocator, capacities, and
session composition.

Transfer ownership remains explicit and fallible. A rejected or implicitly
dropped receive retains queue authority instead of silently recycling detached
Pool storage. Queue recovery resolves storage through the private locator and
uses the queue's preserved source owner, including when a later participant
replaces a dead reaper. Reserved, staged, published, and claimed process-death
cuts are covered. A native nested process cut also kills the first reaper after
its persisted claim and proves a second reaper can finish the original storage
owner before either participant slot is reused.
