// SPDX-License-Identifier: MIT
//! The rig contract shared by rfcheck (checks it) and weightforge (repairs
//! toward it). One copy, so the checker and the fixer cannot drift.

/// Deform bones are named `DEF-*` (rigforge/Rigify convention).
pub const DEFORM_PREFIX: &str = "DEF-";

/// Game-rig limit: at most 4 nonzero influences per vertex.
pub const MAX_INFLUENCES: usize = 4;

/// Weight sums must land within this distance of 1.0.
pub const WEIGHT_TOL: f64 = 1e-3;

/// Mesh-name words that mark a detachable piece (cape, hair, ...): pieces
/// are remeshed separately and must follow their own bones.
pub const PIECE_WORDS: &[&str] = &["cape", "cloak", "hair", "ponytail", "braid"];

pub fn is_deform_bone(name: &str) -> bool {
    name.starts_with(DEFORM_PREFIX)
}

/// Case-insensitive [`PIECE_WORDS`] match on a mesh name.
pub fn is_piece_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    PIECE_WORDS.iter().any(|w| lower.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_deform_bone("DEF-spine.001"));
        assert!(!is_deform_bone("ORG-spine"));
        assert!(is_piece_name("Hero_Cape"));
        assert!(is_piece_name("PONYTAIL"));
        assert!(!is_piece_name("Body"));
    }
}
