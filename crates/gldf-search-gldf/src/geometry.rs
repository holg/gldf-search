//! Map GLDF `<Variant><Mountings>` to schema mounting types.
//!
//! The XSD `<Mountings>` element wraps four optional place-children
//! (`Ceiling`, `Wall`, `WorkingPlane`, `Ground`). Each place has a
//! sub-set of typed children (`Recessed`, `SurfaceMounted`,
//! `Pendant`, `FreeStanding`, `PoleTop`, `PoleIntegrated`).
//!
//! This module reads the typed `gldf-rs` structures and assigns:
//! - [`MountingPlace`] from which `<Mountings>` child is present,
//! - [`MountingType`] from which sub-child is present under that place,
//! - `recessed_depth_mm` (XSD `@recessedDepth`, mm) when the variant
//!   is recessed.
//!
//! Order of precedence when more than one branch is populated (a
//! corpus quirk, rare): Ceiling > Wall > WorkingPlane > Ground for
//! place; first-non-`None` sub-child for type.

use gldf_rs::gldf::Variant;
use gldf_search_schema::doc::{MountingPlace, MountingType};

/// Resolved mounting fields for one variant. The extractor pushes
/// these straight onto [`VariantDoc`](gldf_search_schema::doc::VariantDoc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountingFields {
    /// Top-level `<Mountings>` child the variant declared.
    pub place: MountingPlace,
    /// Inner child element under the mounting place.
    pub kind: MountingType,
    /// XSD `@recessedDepth` in millimetres. Only set when
    /// `kind == MountingType::Recessed`. Always `None` otherwise so
    /// downstream filters don't have to second-guess.
    pub recessed_depth_mm: Option<u32>,
}

impl MountingFields {
    /// What you get when `<Mountings>` is missing or empty.
    pub const UNKNOWN: Self = Self {
        place: MountingPlace::Unknown,
        kind: MountingType::Unknown,
        recessed_depth_mm: None,
    };
}

/// Read mounting data out of one `<Variant>`.
pub fn variant_mounting(v: &Variant) -> MountingFields {
    let Some(m) = v.mountings.as_ref() else {
        return MountingFields::UNKNOWN;
    };

    // Place first. The XSD permits only one child to be present in
    // a valid GLDF; we still ladder defensively (corpus is messy).
    if let Some(c) = m.ceiling.as_ref() {
        // Ceiling sub-types: Recessed | SurfaceMounted | Pendant.
        if let Some(r) = c.recessed.as_ref() {
            return MountingFields {
                place: MountingPlace::Ceiling,
                kind: MountingType::Recessed,
                recessed_depth_mm: Some(r.recessed_depth.max(0) as u32),
            };
        }
        if c.surface_mounted.is_some() {
            return MountingFields {
                place: MountingPlace::Ceiling,
                kind: MountingType::SurfaceMounted,
                recessed_depth_mm: None,
            };
        }
        if c.pendant.is_some() {
            return MountingFields {
                place: MountingPlace::Ceiling,
                kind: MountingType::Pendant,
                recessed_depth_mm: None,
            };
        }
        return MountingFields {
            place: MountingPlace::Ceiling,
            kind: MountingType::Unknown,
            recessed_depth_mm: None,
        };
    }

    if let Some(w) = m.wall.as_ref() {
        // Wall sub-types: Recessed | SurfaceMounted.
        if let Some(r) = w.recessed.as_ref() {
            return MountingFields {
                place: MountingPlace::Wall,
                kind: MountingType::Recessed,
                recessed_depth_mm: Some(r.recessed_depth.max(0) as u32),
            };
        }
        if w.surface_mounted.is_some() {
            return MountingFields {
                place: MountingPlace::Wall,
                kind: MountingType::SurfaceMounted,
                recessed_depth_mm: None,
            };
        }
        return MountingFields {
            place: MountingPlace::Wall,
            kind: MountingType::Unknown,
            recessed_depth_mm: None,
        };
    }

    if let Some(wp) = m.working_plane.as_ref() {
        // WorkingPlane sub-types: FreeStanding.
        if wp.free_standing.is_some() {
            return MountingFields {
                place: MountingPlace::WorkingPlane,
                kind: MountingType::FreeStanding,
                recessed_depth_mm: None,
            };
        }
        return MountingFields {
            place: MountingPlace::WorkingPlane,
            kind: MountingType::Unknown,
            recessed_depth_mm: None,
        };
    }

    if let Some(g) = m.ground.as_ref() {
        // Ground sub-types: PoleTop | PoleIntegrated | FreeStanding
        // | SurfaceMounted | Recessed.
        if let Some(r) = g.recessed.as_ref() {
            return MountingFields {
                place: MountingPlace::Ground,
                kind: MountingType::Recessed,
                recessed_depth_mm: Some(r.recessed_depth.max(0) as u32),
            };
        }
        if g.pole_top.is_some() {
            return MountingFields {
                place: MountingPlace::Ground,
                kind: MountingType::PoleTop,
                recessed_depth_mm: None,
            };
        }
        if g.pole_integrated.is_some() {
            return MountingFields {
                place: MountingPlace::Ground,
                kind: MountingType::PoleIntegrated,
                recessed_depth_mm: None,
            };
        }
        if g.free_standing.is_some() {
            return MountingFields {
                place: MountingPlace::Ground,
                kind: MountingType::FreeStanding,
                recessed_depth_mm: None,
            };
        }
        if g.surface_mounted.is_some() {
            return MountingFields {
                place: MountingPlace::Ground,
                kind: MountingType::SurfaceMounted,
                recessed_depth_mm: None,
            };
        }
        return MountingFields {
            place: MountingPlace::Ground,
            kind: MountingType::Unknown,
            recessed_depth_mm: None,
        };
    }

    MountingFields::UNKNOWN
}
