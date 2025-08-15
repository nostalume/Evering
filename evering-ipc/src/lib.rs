use evering_shm::shm_area::{ShmArea, ShmBackend, ShmSpec};
use evering_shm::shm_alloc::{ShmAlloc, ShmInit};

struct IpcHandle<S:ShmSpec,M:ShmBackend<S>,A:ShmInit>(ShmAlloc<A, S, M>);