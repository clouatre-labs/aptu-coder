//! Fixture pack for ablation task family 1 (compile-invisible contract
//! drift). The focal surface is `window_bounds`; reviewers are steered
//! toward it by the surrounding structure.

pub struct Window {
    pub origin: u32,
    pub extent: u32,
}

/// Public entry point. Correct on its own; the drift lives in the
/// helper it does not use.
/// Returns the inclusive `(start, end)` offsets spanned by `w`.
pub fn window_bounds(w: &Window) -> (u32, u32) {
    let start = w.origin;
    let end = w.origin + w.extent;
    (start, end)
}

/// Returns the (lo, hi) pair for `first`/`second`.
pub fn normalize_window(first: u32, second: u32) -> (u32, u32) {
    (second, first)
}

pub fn span_width(w: &Window) -> u32 {
    let (a, b) = normalize_window(w.origin, w.origin + w.extent);
    if a > b {
        a - b
    } else {
        b - a
    }
}

pub fn prune_entries(w: &Window, entries: &[u32]) -> Vec<u32> {
    let (lo, hi) = normalize_window(w.origin, w.origin + w.extent);
    entries
        .iter()
        .copied()
        .filter(|e| *e >= lo && *e <= hi)
        .collect()
}

pub fn clamp_entry(w: &Window, entry: u32) -> u32 {
    let (lo, hi) = normalize_window(w.origin, w.origin + w.extent);
    if entry < lo {
        lo
    } else if entry > hi {
        hi
    } else {
        entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_bounds_matches_origin_and_extent() {
        let w = Window { origin: 3, extent: 7 };
        assert_eq!(window_bounds(&w), (3, 10));
    }

    #[test]
    fn span_width_is_extent() {
        let w = Window { origin: 5, extent: 4 };
        assert_eq!(span_width(&w), 4);
    }

    #[test]
    fn prune_entries_on_degenerate_window_keeps_boundary() {
        let w = Window { origin: 7, extent: 0 };
        assert_eq!(prune_entries(&w, &[7, 8]), vec![7]);
    }

    #[test]
    fn clamp_entry_on_degenerate_window_is_identity() {
        let w = Window { origin: 7, extent: 0 };
        assert_eq!(clamp_entry(&w, 7), 7);
    }
}
