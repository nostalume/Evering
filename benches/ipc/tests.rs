//! The claims of the `ipc-study` target, one module per thing they are about. A leg of the
//! study is a file here, so the question a run answers is chosen by opening a module
//! rather than by searching a region of one file.
//!
//! Two rules hold the split honest:
//!
//! - A body was moved, not edited. Visibility is the only thing an item here gained
//!   (`pub(super)` on the ten items another area measures with), and no claim was
//!   added, renamed or dropped: the `#[test]` and `fn` inventories of this tree are
//!   what the pre-split `tests.rs` reported.
//! - Nothing in this module is a fixture for production code. The instruments that
//!   outlive the study are the ones the study imports — `boundary`, `drive`, `study` —
//!   and they stay siblings of this module rather than parts of it.

mod boundary_cost;
mod driver_window;
mod evidence_attribution;
mod evidence_generations;
mod geometry_claims;
mod mechanism_specs;
mod membership_schedule;
mod pilot_calibration;
mod publication_authority;
mod record_round_trips;
mod trailer_witness;
mod transport_framing;
