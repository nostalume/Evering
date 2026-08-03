#![cfg(feature = "std")]

#[cfg(all(feature = "map", unix))]
pub mod unix;
#[cfg(all(feature = "map", windows))]
pub mod windows;

#[cfg(all(feature = "notify", target_os = "linux"))]
#[path = "os/eventfd.rs"]
mod event;
#[cfg(all(feature = "notify", unix, not(target_os = "linux")))]
#[path = "os/pipe.rs"]
mod event;
#[cfg(all(feature = "notify", windows))]
#[path = "os/event.rs"]
mod event;

#[cfg(any(all(feature = "notify", unix), all(feature = "notify", windows)))]
pub use event::{Event, Ring, event};

#[cfg(all(feature = "process", unix))]
#[path = "os/unix/handoff.rs"]
mod handoff;
#[cfg(all(feature = "process", windows))]
#[path = "os/windows/handoff.rs"]
mod handoff;
#[cfg(all(feature = "process", any(unix, windows)))]
pub use handoff::{Handoff, Shared, shared};

#[cfg(all(feature = "process", any(unix, windows)))]
pub struct ReceivedResources {
    pub bootstrap: crate::process::Bootstrap,
    pub mapping: Shared,
    pub event: Event,
    pub ring: Ring,
}
