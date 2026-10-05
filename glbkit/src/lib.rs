// SPDX-License-Identifier: MIT
//! GLB building blocks shared by the factory's Rust GLB tools (rfcheck,
//! weightforge, wrapforge).
//!
//! - [`container`]: the glTF 2.0 binary container (read and write).
//! - [`accessor`]: accessor layout resolution and bounds-checked element
//!   reads/writes in the BIN chunk.
//! - [`rig`]: the rig contract constants every tool must agree on.
//!
//! No dependencies and no JSON type: callers parse the JSON chunk with
//! whatever they already use and hand the accessor fields in as plain
//! values. Corrupt input is always an error, never a panic.
//!
//! retopoforge keeps its own reader on purpose: it is a byte-exact port of
//! cgltf pinned by a differential oracle.

pub mod accessor;
pub mod container;
pub mod rig;
