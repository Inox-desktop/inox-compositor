//! Compositor-level input interpretation.
//!
//! The heavy lifting — keymap, modifier tracking and focus delivery — is done by
//! Smithay's seat implementation. This module holds the small, pure decision
//! layer that turns a key press into a compositor [`Action`], which keeps key
//! bindings independent of the event loop and unit testable.

use smithay::input::keyboard::{Keysym, ModifiersState, keysyms};

/// A compositor action triggered by a key binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Move the keyboard focus to the next window in layout order.
    FocusNext,
    /// Move the keyboard focus to the previous window in layout order.
    FocusPrev,
    /// Ask the compositor to shut down cleanly.
    Quit,
}

/// Resolve a key press into a compositor [`Action`].
///
/// Bindings are matched on the *modified* keysym, so `Alt+Tab` and
/// `Alt+Shift+Tab` are distinguished by the modifier state rather than by
/// different key symbols. Returns `None` when the key is not bound, in which
/// case the event is forwarded to the focused client unchanged.
pub fn action_for_key(sym: Keysym, modifiers: &ModifiersState) -> Option<Action> {
    if modifiers.alt && sym == Keysym::new(keysyms::KEY_Tab) {
        return Some(if modifiers.shift {
            Action::FocusPrev
        } else {
            Action::FocusNext
        });
    }

    if modifiers.logo && sym == Keysym::new(keysyms::KEY_q) {
        return Some(Action::Quit);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> Keysym {
        Keysym::new(keysyms::KEY_Tab)
    }

    fn q() -> Keysym {
        Keysym::new(keysyms::KEY_q)
    }

    #[test]
    fn alt_tab_focuses_the_next_window() {
        let modifiers = ModifiersState {
            alt: true,
            ..ModifiersState::default()
        };

        assert_eq!(action_for_key(tab(), &modifiers), Some(Action::FocusNext));
    }

    #[test]
    fn alt_shift_tab_focuses_the_previous_window() {
        let modifiers = ModifiersState {
            alt: true,
            shift: true,
            ..ModifiersState::default()
        };

        assert_eq!(action_for_key(tab(), &modifiers), Some(Action::FocusPrev));
    }

    #[test]
    fn super_q_quits() {
        let modifiers = ModifiersState {
            logo: true,
            ..ModifiersState::default()
        };

        assert_eq!(action_for_key(q(), &modifiers), Some(Action::Quit));
    }

    #[test]
    fn unbound_keys_have_no_action() {
        assert_eq!(action_for_key(tab(), &ModifiersState::default()), None);
        assert_eq!(action_for_key(q(), &ModifiersState::default()), None);

        let shift_only = ModifiersState {
            shift: true,
            ..ModifiersState::default()
        };
        assert_eq!(
            action_for_key(Keysym::new(keysyms::KEY_a), &shift_only),
            None
        );
    }
}
