use core::{mem, sync::atomic::{AtomicBool, Ordering}, task::LocalWaker};

use slab::Slab;
