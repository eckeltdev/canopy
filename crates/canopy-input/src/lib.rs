//! Canopy input model: pure text-editing logic and a focus model.
//!
//! This crate is the keyboard half of Canopy's event story. It deliberately holds
//! **no** platform code and touches **no** renderer or DOM — it only turns a key
//! press into a new string and tracks which node is focused. The window/host layer
//! (the orchestrator) is responsible for translating a platform key event into a
//! [`Key`] and calling the editing primitive on the focused input; this crate just
//! supplies those primitives so the policy is identical on every backend.
//!
//! Why a separate [`Key`] enum instead of reusing [`canopy_protocol::EventPayload`]?
//! The protocol's `Text` payload carries an interned [`canopy_protocol::StrId`], not
//! a borrowed `&str` — by design, since the wire never ships raw strings on the hot
//! path. So the actual character cannot be recovered from a decoded payload alone.
//! [`edit`] still accepts an [`EventPayload`] for the events it *can* interpret
//! (key codes), and the richer character path goes through [`Key`] / [`apply`].
//!
//! `no_std` + `alloc`; everything here is a pure function or a tiny value type, so it
//! is trivially testable and runs unchanged from a desktop host to a bare-metal loop.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;

use canopy_protocol::{EventPayload, NodeId};

/// USB-HID-style key code for Backspace, as it appears in
/// [`EventPayload::Key`]'s `code`. The host maps its platform key codes onto this
/// small set before handing an event to [`edit`]; only the keys the editor acts on
/// need a stable value.
pub const KEY_BACKSPACE: u32 = 0x08;
/// Key code for Enter / Return. See [`KEY_BACKSPACE`].
pub const KEY_ENTER: u32 = 0x0D;

/// A single editing intent against a text buffer.
///
/// This is the smallest model that covers a one-line text field: append a character,
/// delete the last one, or submit. The host produces a [`Key`] from a platform event
/// (a typed character becomes [`Key::Char`]; Backspace/Enter become their variants)
/// and feeds it to [`apply`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// A printable character to append at the end of the buffer.
    Char(char),
    /// Delete the last character (no-op on an empty buffer).
    Backspace,
    /// Commit / submit. The buffer is unchanged; a host watching for submit can
    /// detect this key separately (see [`Key::is_submit`]).
    Enter,
}

impl Key {
    /// Whether this key signals a submit (i.e. it is [`Key::Enter`]). The editor
    /// treats Enter as a no-op on the text itself; a host that wants "commit on
    /// Enter" semantics checks this.
    pub fn is_submit(self) -> bool {
        matches!(self, Key::Enter)
    }
}

/// Apply one [`Key`] to `current`, returning the resulting buffer.
///
/// Pure and total: it never panics and always returns a fresh [`String`].
///
/// - [`Key::Char(c)`](Key::Char) appends `c`.
/// - [`Key::Backspace`] pops the last `char` (UTF-8 aware; a no-op when empty).
/// - [`Key::Enter`] leaves the text unchanged (submit is signalled, not edited in).
///
/// ```
/// use canopy_input::{apply, Key};
/// let s = apply("", Key::Char('a'));
/// let s = apply(&s, Key::Char('b'));
/// let s = apply(&s, Key::Backspace);
/// assert_eq!(s, "a");
/// ```
pub fn apply(current: &str, key: Key) -> String {
    let mut out = String::from(current);
    match key {
        Key::Char(c) => out.push(c),
        Key::Backspace => {
            out.pop();
        }
        Key::Enter => {}
    }
    out
}

/// Interpret an event against `current`, returning the new text or [`None`] when the
/// event does not change the buffer.
///
/// This is the [`EventPayload`]-facing entry point for hosts that route raw decoded
/// events here. It can only act on what an [`EventPayload`] actually carries:
///
/// - [`EventPayload::Key`] with `code == `[`KEY_BACKSPACE`] deletes the last char,
///   returning the new text (or [`None`] if the buffer was already empty, since
///   nothing changed).
/// - [`EventPayload::Key`] with `code == `[`KEY_ENTER`] is a submit and returns
///   [`None`] (no text change).
/// - Every other payload returns [`None`] — including [`EventPayload::Text`], whose
///   [`canopy_protocol::StrId`] cannot be resolved to characters here. To type a
///   character, the host builds a [`Key::Char`] and calls [`apply`] instead.
///
/// ```
/// use canopy_input::{edit, KEY_BACKSPACE};
/// use canopy_protocol::EventPayload;
/// let payload = EventPayload::Key { code: KEY_BACKSPACE, mods: 0 };
/// assert_eq!(edit("ab", &payload).as_deref(), Some("a"));
/// assert_eq!(edit("", &payload), None); // nothing to delete
/// ```
pub fn edit(current: &str, event: &EventPayload) -> Option<String> {
    match event {
        EventPayload::Key { code, .. } if *code == KEY_BACKSPACE => {
            if current.is_empty() {
                None
            } else {
                Some(apply(current, Key::Backspace))
            }
        }
        // Enter is a submit, not a text edit; other payloads carry no usable char.
        _ => None,
    }
}

/// Which node currently receives keyboard input.
///
/// A focus model is just a single optional [`NodeId`]: at most one node is focused
/// at a time, and the host directs key events to it. The orchestrator owns one of
/// these (or the [`canopy_view`](../canopy_view/index.html) `App` does, on its
/// behalf) and updates it on click / tab / programmatic focus.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Focus {
    node: Option<NodeId>,
}

impl Focus {
    /// A new focus model with nothing focused.
    pub fn new() -> Self {
        Self { node: None }
    }

    /// Focus `node`, replacing any previous focus.
    pub fn set(&mut self, node: NodeId) {
        self.node = Some(node);
    }

    /// The currently focused node, if any.
    pub fn get(&self) -> Option<NodeId> {
        self.node
    }

    /// Clear focus so no node receives keys.
    pub fn clear(&mut self) {
        self.node = None;
    }

    /// Whether `node` is the focused node.
    pub fn is_focused(&self, node: NodeId) -> bool {
        self.node == Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_two_chars_then_backspace_over_empty_yields_first() {
        // 'a', 'b', Backspace over "" -> "a" (the task's golden sequence).
        let s = apply("", Key::Char('a'));
        assert_eq!(s, "a");
        let s = apply(&s, Key::Char('b'));
        assert_eq!(s, "ab");
        let s = apply(&s, Key::Backspace);
        assert_eq!(s, "a");
    }

    #[test]
    fn backspace_on_empty_is_a_noop() {
        assert_eq!(apply("", Key::Backspace), "");
    }

    #[test]
    fn enter_does_not_change_text_but_signals_submit() {
        assert_eq!(apply("hello", Key::Enter), "hello");
        assert!(Key::Enter.is_submit());
        assert!(!Key::Char('x').is_submit());
        assert!(!Key::Backspace.is_submit());
    }

    #[test]
    fn backspace_is_utf8_aware() {
        // Popping must drop a whole `char`, not a single byte.
        let s = apply("é", Key::Backspace);
        assert_eq!(s, "");
        let s = apply("aé", Key::Backspace);
        assert_eq!(s, "a");
    }

    #[test]
    fn char_appends_multibyte() {
        let s = apply("a", Key::Char('é'));
        assert_eq!(s, "aé");
    }

    #[test]
    fn edit_backspace_payload_deletes_last_char() {
        let payload = EventPayload::Key {
            code: KEY_BACKSPACE,
            mods: 0,
        };
        assert_eq!(edit("ab", &payload).as_deref(), Some("a"));
        assert_eq!(edit("a", &payload).as_deref(), Some(""));
    }

    #[test]
    fn edit_backspace_on_empty_returns_none() {
        let payload = EventPayload::Key {
            code: KEY_BACKSPACE,
            mods: 0,
        };
        assert_eq!(edit("", &payload), None);
    }

    #[test]
    fn edit_enter_and_other_payloads_return_none() {
        let enter = EventPayload::Key {
            code: KEY_ENTER,
            mods: 0,
        };
        assert_eq!(edit("hi", &enter), None);
        assert_eq!(edit("hi", &EventPayload::None), None);
        assert_eq!(
            edit(
                "hi",
                &EventPayload::Pointer {
                    x: 0.0,
                    y: 0.0,
                    button: 0
                }
            ),
            None
        );
        // A Text payload carries an unresolvable StrId, so it cannot edit here.
        assert_eq!(
            edit(
                "hi",
                &EventPayload::Text {
                    text: canopy_protocol::StrId::new(0)
                }
            ),
            None
        );
    }

    #[test]
    fn focus_set_get_clear() {
        let mut f = Focus::new();
        assert_eq!(f.get(), None);
        assert_eq!(Focus::default(), f);

        let n = NodeId::new(7);
        f.set(n);
        assert_eq!(f.get(), Some(n));
        assert!(f.is_focused(n));
        assert!(!f.is_focused(NodeId::new(8)));

        // Setting again replaces the focus.
        let m = NodeId::new(9);
        f.set(m);
        assert_eq!(f.get(), Some(m));
        assert!(!f.is_focused(n));

        f.clear();
        assert_eq!(f.get(), None);
        assert!(!f.is_focused(m));
    }
}
