//! Tiling layout for managed windows.
//!
//! The module is pure geometry: it holds no Smithay state and performs no I/O,
//! which keeps the algorithm unit testable and property testable. PRD §13 makes
//! the tiling layout part of the M2 acceptance criteria and explicitly requires
//! it to be verified with `proptest`.
//!
//! The layout is a two-column master/stack arrangement: the first
//! [`LayoutConfig::master_count`] windows tile the left (master) column, every
//! remaining window tiles the right (stack) column. A gap separates the work
//! area edge from the tiles and neighbouring tiles from each other.

use smithay::utils::{Logical, Rectangle};

/// Smallest accepted percentage of the work area width given to the master column.
pub const MASTER_RATIO_MIN: u8 = 10;

/// Largest accepted percentage of the work area width given to the master column.
pub const MASTER_RATIO_MAX: u8 = 90;

/// Parameters of the master/stack tiling layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutConfig {
    /// Percentage (clamped to [`MASTER_RATIO_MIN`]..=[`MASTER_RATIO_MAX`]) of the
    /// work area width assigned to the master column.
    pub master_ratio: u8,
    /// Number of windows tiled in the master column before the stack is used.
    pub master_count: usize,
    /// Gap in logical pixels between the work area edge and its tiles, and
    /// between neighbouring tiles.
    pub gap: i32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            master_ratio: 60,
            master_count: 1,
            gap: 8,
        }
    }
}

impl LayoutConfig {
    /// Clamp every field into its accepted range.
    ///
    /// Out-of-range values are accepted rather than rejected so that a bad
    /// configuration degrades into a usable layout instead of aborting startup.
    pub fn sanitised(self) -> Self {
        Self {
            master_ratio: self.master_ratio.clamp(MASTER_RATIO_MIN, MASTER_RATIO_MAX),
            master_count: self.master_count.max(1),
            gap: self.gap.max(0),
        }
    }

    /// Arrange `count` tiles inside `work_area`, master column first.
    ///
    /// The returned rectangles are in layout order (master window first) and are
    /// guaranteed to be non-negative, mutually non-overlapping and contained in
    /// `work_area`, including for degenerate work areas smaller than the gap.
    pub fn arrange(
        &self,
        work_area: Rectangle<i32, Logical>,
        count: usize,
    ) -> Vec<Rectangle<i32, Logical>> {
        if count == 0 {
            return Vec::new();
        }

        let config = self.sanitised();
        let inner = inner_area(work_area, config.gap);
        let master_count = config.master_count.min(count);
        let stack_count = count - master_count;

        if stack_count == 0 {
            return split_rows(inner, count, config.gap)
                .into_iter()
                .map(|(y, height)| rect((inner.loc.x, y), (inner.size.w, height)))
                .collect();
        }

        let master_width = inner.size.w.saturating_mul(i32::from(config.master_ratio)) / 100;
        let stack_x = (inner.loc.x + master_width + config.gap).min(inner.loc.x + inner.size.w);
        let stack_width = (inner.loc.x + inner.size.w - stack_x).max(0);

        let mut tiles: Vec<Rectangle<i32, Logical>> = split_rows(inner, master_count, config.gap)
            .into_iter()
            .map(|(y, height)| rect((inner.loc.x, y), (master_width, height)))
            .collect();

        tiles.extend(
            split_rows(inner, stack_count, config.gap)
                .into_iter()
                .map(|(y, height)| rect((stack_x, y), (stack_width, height))),
        );

        tiles
    }
}

/// Build a rectangle from a location and a size, both given as `i32` pairs.
fn rect(loc: (i32, i32), size: (i32, i32)) -> Rectangle<i32, Logical> {
    Rectangle::new(loc.into(), size.into())
}

/// The work area shrunk by `gap`, clamped so it stays inside the work area.
fn inner_area(work_area: Rectangle<i32, Logical>, gap: i32) -> Rectangle<i32, Logical> {
    let right = work_area.loc.x + work_area.size.w;
    let bottom = work_area.loc.y + work_area.size.h;

    let left = (work_area.loc.x + gap).min(right);
    let top = (work_area.loc.y + gap).min(bottom);
    let inner_right = (right - gap).max(work_area.loc.x);
    let inner_bottom = (bottom - gap).max(work_area.loc.y);

    rect(
        (left, top),
        ((inner_right - left).max(0), (inner_bottom - top).max(0)),
    )
}

/// Split `area` vertically into `parts` rows separated by `gap`.
///
/// Returns `(y, height)` pairs in top-to-bottom order. Heights are never
/// negative: when the gap leaves no room, every tile collapses to zero height at
/// the area's top edge rather than overflowing.
fn split_rows(area: Rectangle<i32, Logical>, parts: usize, gap: i32) -> Vec<(i32, i32)> {
    split_axis(area.loc.y, area.size.h, parts, gap)
}

/// Split `total` pixels into `parts` segments separated by `gap`, starting at `origin`.
fn split_axis(origin: i32, total: i32, parts: usize, gap: i32) -> Vec<(i32, i32)> {
    if parts == 0 {
        return Vec::new();
    }

    let separators = gap.saturating_mul(parts as i32 - 1);
    let usable = total.saturating_sub(separators);

    if usable <= 0 {
        return vec![(origin, 0); parts];
    }

    let base = usable / parts as i32;
    let remainder = usable % parts as i32;
    let mut segments = Vec::with_capacity(parts);
    let mut cursor = origin;

    for index in 0..parts {
        let size = base + i32::from((index as i32) < remainder);
        segments.push((cursor, size));
        cursor += size + gap;
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: i32, h: i32) -> Rectangle<i32, Logical> {
        rect((0, 0), (w, h))
    }

    #[test]
    fn no_windows_produce_no_tiles() {
        assert!(
            LayoutConfig::default()
                .arrange(area(1280, 720), 0)
                .is_empty()
        );
    }

    #[test]
    fn a_single_window_fills_the_inner_work_area() {
        let config = LayoutConfig {
            gap: 10,
            ..LayoutConfig::default()
        };

        let tiles = config.arrange(area(1000, 500), 1);

        assert_eq!(tiles, vec![rect((10, 10), (980, 480))]);
    }

    #[test]
    fn a_single_window_fills_the_work_area_without_a_gap() {
        let config = LayoutConfig {
            gap: 0,
            ..LayoutConfig::default()
        };

        assert_eq!(config.arrange(area(800, 600), 1), vec![area(800, 600)]);
    }

    #[test]
    fn two_windows_split_into_master_and_stack_columns() {
        let config = LayoutConfig {
            master_ratio: 50,
            master_count: 1,
            gap: 0,
        };

        let tiles = config.arrange(area(1000, 400), 2);

        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0], rect((0, 0), (500, 400)));
        assert_eq!(tiles[1], rect((500, 0), (500, 400)));
    }

    #[test]
    fn extra_windows_stack_vertically() {
        let config = LayoutConfig {
            master_ratio: 50,
            master_count: 1,
            gap: 0,
        };

        let tiles = config.arrange(area(1000, 400), 3);

        assert_eq!(tiles[0], rect((0, 0), (500, 400)));
        assert_eq!(tiles[1], rect((500, 0), (500, 200)));
        assert_eq!(tiles[2], rect((500, 200), (500, 200)));
    }

    #[test]
    fn multiple_master_windows_share_the_master_column() {
        let config = LayoutConfig {
            master_ratio: 50,
            master_count: 2,
            gap: 0,
        };

        let tiles = config.arrange(area(1000, 400), 3);

        assert_eq!(tiles[0], rect((0, 0), (500, 200)));
        assert_eq!(tiles[1], rect((0, 200), (500, 200)));
        assert_eq!(tiles[2], rect((500, 0), (500, 400)));
    }

    #[test]
    fn gaps_separate_the_tiles() {
        let config = LayoutConfig {
            master_ratio: 50,
            master_count: 1,
            gap: 10,
        };

        let tiles = config.arrange(area(1000, 400), 2);
        let master = tiles[0];
        let stack = tiles[1];

        // The gap shows up both at the edges and between the columns.
        assert_eq!(master.loc.x, 10);
        assert_eq!(stack.loc.x - (master.loc.x + master.size.w), 10);
    }

    #[test]
    fn the_ratio_is_clamped_to_its_range() {
        let extreme = LayoutConfig {
            master_ratio: 100,
            master_count: 1,
            gap: 0,
        };

        let tiles = extreme.arrange(area(1000, 400), 2);

        // The stack column keeps a slice even when the ratio is out of range.
        assert!(tiles[1].size.w > 0);
    }

    #[test]
    fn a_work_area_smaller_than_the_gap_still_yields_tiles() {
        let config = LayoutConfig {
            gap: 50,
            ..LayoutConfig::default()
        };

        let tiles = config.arrange(area(10, 10), 3);

        assert_eq!(tiles.len(), 3);
        for tile in tiles {
            assert!(tile.size.w >= 0 && tile.size.h >= 0);
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// A strategy producing arbitrary work areas, including degenerate ones.
    fn work_area() -> impl Strategy<Value = Rectangle<i32, Logical>> {
        (-2_000i32..2_000, -2_000i32..2_000, 0i32..3_000, 0i32..3_000)
            .prop_map(|(x, y, w, h)| rect((x, y), (w, h)))
    }

    fn layout_config() -> impl Strategy<Value = LayoutConfig> {
        (0u8..=120, 0usize..6, 0i32..64).prop_map(|(master_ratio, master_count, gap)| {
            LayoutConfig {
                master_ratio,
                master_count,
                gap,
            }
        })
    }

    proptest! {
        #[test]
        fn one_tile_per_window(
            work_area in work_area(),
            count in 0usize..12,
            config in layout_config(),
        ) {
            prop_assert_eq!(config.arrange(work_area, count).len(), count);
        }

        #[test]
        fn tiles_are_contained_in_the_work_area(
            work_area in work_area(),
            count in 0usize..12,
            config in layout_config(),
        ) {
            for tile in config.arrange(work_area, count) {
                prop_assert!(tile.size.w >= 0);
                prop_assert!(tile.size.h >= 0);
                prop_assert!(tile.loc.x >= work_area.loc.x);
                prop_assert!(tile.loc.y >= work_area.loc.y);
                prop_assert!(tile.loc.x + tile.size.w <= work_area.loc.x + work_area.size.w);
                prop_assert!(tile.loc.y + tile.size.h <= work_area.loc.y + work_area.size.h);
            }
        }

        #[test]
        fn tiles_never_overlap(
            work_area in work_area(),
            count in 0usize..12,
            config in layout_config(),
        ) {
            let tiles = config.arrange(work_area, count);
            for (index, tile) in tiles.iter().enumerate() {
                for other in tiles.iter().skip(index + 1) {
                    prop_assert!(!tile.overlaps(*other), "{tile:?} overlaps {other:?}");
                }
            }
        }

        #[test]
        fn the_master_column_is_left_of_the_stack(
            work_area in work_area(),
            count in 1usize..12,
            config in layout_config(),
        ) {
            let config = config.sanitised();
            let tiles = config.arrange(work_area, count);
            let master_count = config.master_count.min(count);

            if master_count < count {
                let master_right = tiles[..master_count]
                    .iter()
                    .map(|tile| tile.loc.x + tile.size.w)
                    .max()
                    .unwrap_or(0);
                for stack_tile in &tiles[master_count..] {
                    prop_assert!(stack_tile.loc.x >= master_right);
                }
            }
        }
    }
}
