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
