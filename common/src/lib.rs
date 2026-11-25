//! Code shared by the six-DOF test stand firmware (`firmware/`, `no_std`) and the
//! web UI (`web/`, wasm):
//! - [`interface`]: the postcard-rpc interface (endpoints, topics, message types).
//! - [`geometry`]: platform geometry, load cell data, the ideal wrench matrix and
//!   the default calibration (source: `reference/geometry.md`).
//! - [`calibration`]: applying a calibration (`w = C · (c − z)`).

// `no_std` except for the web UI (`use-std`) and unit tests (which run on the
// host and may use `std`, e.g. `f64::sqrt`).
#![cfg_attr(not(any(test, feature = "use-std")), no_std)]

pub mod calibration;
pub mod geometry;
pub mod interface;
