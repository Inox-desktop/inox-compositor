//! Managed windows and the order in which they are tiled.
//!
//! The module is pure state: it never touches Smithay or the Wayland display, so
//! the window bookkeeping (add, remove, focus, layout order) is unit testable and
//! property testable independently of the compositor integration. The Smithay
//! layer in [`crate::state`] owns one [`Workspace`] and maps its [`WindowId`]s to
//! real `desktop::Window`s.

use smithay::utils::{Logical, Rectangle};

use crate::layout::LayoutConfig;

/// Identifier of a window managed by the compositor.
///
/// Identifiers are handed out by [`Workspace`] and stay stable for the lifetime
/// of the window, which lets the Smithay layer correlate a protocol surface with
/// its slot in the layout order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(u64);

impl WindowId {
    /// The raw identifier value.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The ordered set of managed windows plus the focused window.
///
/// The order is the layout order: the window in the first slot is the layout
/// master and every window added later is placed before it, so the most recently
/// added window becomes the master.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    windows: Vec<WindowId>,
    focused: Option<WindowId>,
    next_id: u64,
}

impl Workspace {
    /// Create an empty workspace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a window in the master slot and focus it.
    pub fn add(&mut self) -> WindowId {
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);

        self.windows.insert(0, id);
        self.focused = Some(id);

        id
    }

    /// Remove a window, returning whether it was present.
    ///
    /// When the focused window is removed, focus moves to the window that took
    /// its slot, or to the last window if it was the final one.
    pub fn remove(&mut self, id: WindowId) -> bool {
        let Some(index) = self.windows.iter().position(|candidate| *candidate == id) else {
            return false;
        };

        self.windows.remove(index);

        if self.focused == Some(id) {
            self.focused = self
                .windows
                .get(index)
                .or_else(|| self.windows.last())
                .copied();
        }

        true
    }

    /// Whether the window is managed by this workspace.
    pub fn contains(&self, id: WindowId) -> bool {
        self.windows.contains(&id)
    }

    /// Focus a managed window, returning whether it is tracked.
    pub fn focus(&mut self, id: WindowId) -> bool {
        if self.windows.contains(&id) {
            self.focused = Some(id);
            true
        } else {
            false
        }
    }

    /// Focus the next window in layout order, wrapping around.
    pub fn focus_next(&mut self) -> Option<WindowId> {
        self.cycle(1)
    }

    /// Focus the previous window in layout order, wrapping around.
    pub fn focus_prev(&mut self) -> Option<WindowId> {
        self.cycle(-1)
    }

    /// The managed windows in layout order, master first.
    pub fn windows(&self) -> &[WindowId] {
        &self.windows
    }

    /// The focused window, if any.
    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }

    /// Number of managed windows.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Whether no window is managed.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Compute one tile per managed window, in layout order.
    pub fn arrange(
        &self,
        work_area: Rectangle<i32, Logical>,
        config: LayoutConfig,
    ) -> Vec<(WindowId, Rectangle<i32, Logical>)> {
        config
            .arrange(work_area, self.windows.len())
            .into_iter()
            .zip(self.windows.iter().copied())
            .map(|(tile, id)| (id, tile))
            .collect()
    }

    /// Move the focus by `delta` slots in layout order, wrapping around.
    fn cycle(&mut self, delta: isize) -> Option<WindowId> {
        if self.windows.is_empty() {
            self.focused = None;
            return None;
        }

        let current = self
            .focused
            .and_then(|focused| self.windows.iter().position(|id| *id == focused));

        let next = match current {
            Some(index) => {
                (index as isize + delta).rem_euclid(self.windows.len() as isize) as usize
            }
            None => 0,
        };

        let id = self.windows[next];
        self.focused = Some(id);

        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_windows_assigns_unique_ids_and_focuses_the_newest() {
        let mut workspace = Workspace::new();

        let first = workspace.add();
        let second = workspace.add();

        assert_ne!(first, second);
        assert_eq!(workspace.len(), 2);
        assert_eq!(workspace.focused(), Some(second));
        assert_eq!(workspace.windows(), &[second, first]);
    }

    #[test]
    fn removing_the_focused_window_moves_focus_to_the_next_slot() {
        let mut workspace = Workspace::new();
        let first = workspace.add();
        let second = workspace.add();
        let third = workspace.add();

        // The newest window is the master and holds the focus.
        assert_eq!(workspace.windows(), &[third, second, first]);
        assert_eq!(workspace.focused(), Some(third));

        assert!(workspace.remove(third));
        assert_eq!(workspace.windows(), &[second, first]);
        assert_eq!(workspace.focused(), Some(second));
    }

    #[test]
    fn removing_an_unfocused_window_keeps_the_focus() {
        let mut workspace = Workspace::new();
        let first = workspace.add();
        let second = workspace.add();
        let third = workspace.add();

        assert!(workspace.remove(first));

        assert_eq!(workspace.windows(), &[third, second]);
        assert_eq!(workspace.focused(), Some(third));
    }

    #[test]
    fn removing_an_unknown_window_changes_nothing() {
        let mut workspace = Workspace::new();
        let id = workspace.add();
        let unknown = WindowId(9_999);

        assert!(!workspace.remove(unknown));
        assert_eq!(workspace.len(), 1);
        assert_eq!(workspace.focused(), Some(id));
    }

    #[test]
    fn removing_the_last_window_clears_the_focus() {
        let mut workspace = Workspace::new();
        let id = workspace.add();

        assert!(workspace.remove(id));
        assert!(workspace.is_empty());
        assert_eq!(workspace.focused(), None);
    }

    #[test]
    fn focus_cycles_through_every_window_and_wraps() {
        let mut workspace = Workspace::new();
        let first = workspace.add();
        let second = workspace.add();
        let third = workspace.add();

        // Layout order is [third, second, first]; focus starts on third.
        assert_eq!(workspace.focus_next(), Some(second));
        assert_eq!(workspace.focus_next(), Some(first));
        assert_eq!(workspace.focus_next(), Some(third));
        assert_eq!(workspace.focus_prev(), Some(first));
    }

    #[test]
    fn focus_ignores_untracked_windows() {
        let mut workspace = Workspace::new();
        workspace.add();

        assert!(!workspace.focus(WindowId(42)));
    }

    #[test]
    fn fresh_windows_are_arranged_in_layout_order() {
        let mut workspace = Workspace::new();
        let first = workspace.add();
        let second = workspace.add();

        let config = LayoutConfig {
            gap: 0,
            ..LayoutConfig::default()
        };
        let area = Rectangle::<i32, Logical>::new((0, 0).into(), (100, 100).into());
        let tiles = workspace.arrange(area, config);

        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].0, second);
        assert_eq!(tiles[1].0, first);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn added_ids_are_unique_and_tracked(operations in 1usize..24) {
            let mut workspace = Workspace::new();
            let mut ids = Vec::new();

            for _ in 0..operations {
                let id = workspace.add();
                prop_assert!(!ids.contains(&id));
                ids.push(id);
            }

            prop_assert_eq!(workspace.len(), operations);
            for id in &ids {
                prop_assert!(workspace.contains(*id));
            }
            prop_assert!(workspace.windows().iter().all(|id| ids.contains(id)));
        }

        #[test]
        fn removed_windows_leave_the_focus_valid(operations in 1usize..24) {
            let mut workspace = Workspace::new();
            let mut ids = Vec::new();

            for _ in 0..operations {
                ids.push(workspace.add());
            }

            while let Some(id) = ids.pop() {
                prop_assert!(workspace.remove(id));
                prop_assert_eq!(workspace.len(), ids.len());

                match workspace.focused() {
                    Some(focused) => prop_assert!(workspace.contains(focused)),
                    None => prop_assert!(workspace.is_empty()),
                }
            }
        }

        #[test]
        fn focus_cycling_stays_within_the_window_set(steps in 0usize..40) {
            let mut workspace = Workspace::new();
            for _ in 0..3 {
                workspace.add();
            }

            for step in 0..steps {
                let focused = if step % 2 == 0 {
                    workspace.focus_next()
                } else {
                    workspace.focus_prev()
                };

                let focused = focused.expect("a non-empty workspace always has a focused window");
                prop_assert!(workspace.contains(focused));
                prop_assert_eq!(workspace.focused(), Some(focused));
            }
        }

        #[test]
        fn arranging_yields_one_tile_per_window_in_order(
            count in 0usize..10,
            gap in 0i32..32,
        ) {
            let mut workspace = Workspace::new();
            for _ in 0..count {
                workspace.add();
            }

            let config = LayoutConfig {
                gap,
                ..LayoutConfig::default()
            };
            let area = Rectangle::<i32, Logical>::new((0, 0).into(), (1280, 720).into());
            let tiles = workspace.arrange(area, config);

            prop_assert_eq!(tiles.len(), count);
            let ids: Vec<WindowId> = tiles.iter().map(|(id, _)| *id).collect();
            prop_assert_eq!(ids, workspace.windows());
        }
    }
}
