//! Canopy wire protocol: opaque handles, opcodes, and the batched op-stream codec.
//!
//! `canopy-protocol` is the one contract shared by every Canopy transport
//! (compiled-in native and WASM-sandboxed) and every backend. It is `no_std` +
//! `alloc` with **zero external dependencies**, so it builds unchanged from a
//! desktop host down to a bare-metal target.
//!
//! The op-stream is a flat, batched sequence of typed mutations ([`Op`]). Handles
//! ([`NodeId`], [`StrId`], [`HandlerId`], [`ResId`]) are opaque integers. The host
//! arena mints [`NodeId`]s and validates ownership on every mutating op — that is
//! where the capability / unforgeability guarantee lives. A guest can name only the
//! nodes it was handed.
//!
//! Both transports move the **identical bytes** produced by [`OpEncoder`]; the
//! native path hands them to the host in-process, the WASM path copies them across
//! the sandbox boundary. [`OpReader`] / [`decode_all`] decode them back, which is
//! what makes a single golden conformance test cover both transports.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

/// Version tag carried by every [`Op::BeginBatch`]. Bump on any wire-format change
/// so a host can support a window of protocol versions instead of breaking all
/// guests at once.
pub const PROTOCOL_VERSION: u16 = 1;

macro_rules! handle {
    ($(#[$m:meta])* $name:ident, $repr:ty) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name($repr);

        impl $name {
            /// Wrap a raw value. Construction does not grant access — the host
            /// validates ownership of every handle it receives.
            pub const fn new(raw: $repr) -> Self {
                Self(raw)
            }

            /// The underlying raw value.
            pub const fn raw(self) -> $repr {
                self.0
            }
        }
    };
}

handle!(
    /// Opaque, host-minted node handle. The sentinel [`NodeId::NULL`] means
    /// "no node" (e.g. append, when used as an insert anchor).
    NodeId,
    u64
);
handle!(
    /// Interned-string id. Strings are sent once via [`Op::InternString`] and then
    /// referenced by id, which keeps the hot-path ops tiny.
    StrId,
    u32
);
handle!(
    /// Event-handler id, minted guest-side and echoed back in [`Op::DispatchEvent`].
    HandlerId,
    u32
);
handle!(
    /// Resource (image/font/blob) id.
    ResId,
    u32
);
handle!(
    /// Interned element kind (e.g. box/text/row). Values are assigned by the host
    /// widget registry, not hard-coded in the protocol.
    ElementTag,
    u16
);
handle!(
    /// Interned attribute name id.
    AttrId,
    u16
);
handle!(
    /// Interned CSS-property id.
    PropId,
    u16
);
handle!(
    /// Interned event-kind id (e.g. click/input).
    EventKind,
    u16
);

impl NodeId {
    /// Sentinel meaning "no node" — used as an append anchor in [`Op::InsertBefore`].
    pub const NULL: NodeId = NodeId(u64::MAX);

    /// Whether this is the [`NodeId::NULL`] sentinel.
    pub const fn is_null(self) -> bool {
        self.0 == u64::MAX
    }
}

impl AttrId {
    /// The well-known **id** attribute. A capable-tier guest sets an element's CSS id
    /// via `Op::SetAttribute { attr: AttrId::ID, .. }`; the host retains it (id /
    /// attribute selectors). Reserved so it never collides with host-minted attr ids.
    pub const ID: AttrId = AttrId::new(1);
}

/// A single mutation in the op-stream.
///
/// `BeginBatch`..`EndBatch` brackets are applied by the host atomically, then it
/// runs style → layout → paint once. `DispatchEvent` is the only host→guest op.
#[derive(Clone, PartialEq, Debug)]
pub enum Op {
    /// Open a transaction. Carries the protocol version and a monotonic sequence id.
    BeginBatch {
        /// Protocol version of this batch.
        version: u16,
        /// Monotonic frame/sequence number.
        seq: u32,
    },
    /// Close the current transaction; the host applies it atomically.
    EndBatch,

    /// Create a detached element node.
    CreateElement {
        /// Provisional node handle assigned by the guest reconciler.
        node: NodeId,
        /// Element kind.
        tag: ElementTag,
    },
    /// Create a detached text node referencing an interned string.
    CreateText {
        /// Node handle.
        node: NodeId,
        /// Interned text.
        text: StrId,
    },
    /// Remove a node (and, by host contract, its subtree).
    RemoveNode {
        /// Node to remove.
        node: NodeId,
    },
    /// Insert `child` under `parent` before `anchor` ([`NodeId::NULL`] = append).
    InsertBefore {
        /// Parent node.
        parent: NodeId,
        /// Child node to insert.
        child: NodeId,
        /// Anchor sibling, or [`NodeId::NULL`] to append.
        anchor: NodeId,
    },

    /// Replace a text node's content.
    SetText {
        /// Text node.
        node: NodeId,
        /// New interned text.
        text: StrId,
    },
    /// Set an attribute to an interned value.
    SetAttribute {
        /// Target node.
        node: NodeId,
        /// Attribute name id.
        attr: AttrId,
        /// Interned value.
        value: StrId,
    },
    /// Set one inline style property to an interned value (fed to the style engine).
    SetInlineStyle {
        /// Target node.
        node: NodeId,
        /// Property id.
        prop: PropId,
        /// Interned value.
        value: StrId,
    },
    /// Add a class.
    SetClass {
        /// Target node.
        node: NodeId,
        /// Interned class name.
        class: StrId,
    },
    /// Remove a class.
    RemoveClass {
        /// Target node.
        node: NodeId,
        /// Interned class name.
        class: StrId,
    },
    /// Set an element's CSS local name (e.g. `"div"`, `"button"`).
    ///
    /// [`ElementTag`] is an opaque host-assigned id, not a CSS name; capable tiers
    /// that run a real cascade (Stylo) need the type-selector name, so the guest
    /// declares it here. Constrained tiers that resolve styles author-side never
    /// emit this, and the host simply retains it for whatever style engine wants it.
    SetTagName {
        /// Target node.
        node: NodeId,
        /// Interned CSS local name.
        name: StrId,
    },

    /// Subscribe `node` to an event kind; the host routes matching events to
    /// `handler`. The grant is a capability: the guest only receives events for
    /// nodes it subscribed.
    AddListener {
        /// Target node.
        node: NodeId,
        /// Event kind.
        event: EventKind,
        /// Handler id to dispatch to.
        handler: HandlerId,
    },
    /// Remove a previously added listener.
    RemoveListener {
        /// Target node.
        node: NodeId,
        /// Event kind.
        event: EventKind,
    },

    /// Populate the string table once; later ops reference `id`.
    InternString {
        /// Id being defined.
        id: StrId,
        /// UTF-8 bytes.
        bytes: Vec<u8>,
    },

    /// Host → guest: deliver an event to a handler.
    DispatchEvent {
        /// Handler that was registered.
        handler: HandlerId,
        /// Node the event targeted.
        node: NodeId,
        /// Event data.
        payload: EventPayload,
    },
}

/// Decoded event data delivered to a guest handler.
#[derive(Clone, PartialEq, Debug)]
pub enum EventPayload {
    /// No payload (e.g. a plain click with no extra data).
    None,
    /// Pointer event.
    Pointer {
        /// X in logical pixels.
        x: f32,
        /// Y in logical pixels.
        y: f32,
        /// Button index.
        button: u8,
    },
    /// Keyboard event.
    Key {
        /// Key code.
        code: u32,
        /// Modifier bitflags.
        mods: u8,
    },
    /// Text/input event referencing an interned string.
    Text {
        /// Interned committed text.
        text: StrId,
    },
}

mod tags {
    pub const BEGIN_BATCH: u8 = 0x01;
    pub const END_BATCH: u8 = 0x02;
    pub const CREATE_ELEMENT: u8 = 0x10;
    pub const CREATE_TEXT: u8 = 0x11;
    pub const REMOVE_NODE: u8 = 0x12;
    pub const INSERT_BEFORE: u8 = 0x13;
    pub const SET_TEXT: u8 = 0x14;
    pub const SET_ATTRIBUTE: u8 = 0x15;
    pub const SET_INLINE_STYLE: u8 = 0x16;
    pub const SET_CLASS: u8 = 0x17;
    pub const REMOVE_CLASS: u8 = 0x18;
    pub const ADD_LISTENER: u8 = 0x19;
    pub const REMOVE_LISTENER: u8 = 0x1A;
    pub const INTERN_STRING: u8 = 0x1B;
    pub const SET_TAG_NAME: u8 = 0x1C;
    pub const DISPATCH_EVENT: u8 = 0x80;

    pub const PAYLOAD_NONE: u8 = 0;
    pub const PAYLOAD_POINTER: u8 = 1;
    pub const PAYLOAD_KEY: u8 = 2;
    pub const PAYLOAD_TEXT: u8 = 3;
}

/// Appends [`Op`]s to a little-endian byte buffer.
#[derive(Default)]
pub struct OpEncoder {
    buf: Vec<u8>,
}

impl OpEncoder {
    /// New empty encoder.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Convenience: push a [`Op::BeginBatch`] at the current [`PROTOCOL_VERSION`].
    pub fn begin_batch(&mut self, seq: u32) {
        self.push(&Op::BeginBatch {
            version: PROTOCOL_VERSION,
            seq,
        });
    }

    /// Convenience: push a [`Op::EndBatch`].
    pub fn end_batch(&mut self) {
        self.push(&Op::EndBatch);
    }

    /// Encode one op.
    pub fn push(&mut self, op: &Op) {
        match op {
            Op::BeginBatch { version, seq } => {
                self.w_u8(tags::BEGIN_BATCH);
                self.w_u16(*version);
                self.w_u32(*seq);
            }
            Op::EndBatch => self.w_u8(tags::END_BATCH),
            Op::CreateElement { node, tag } => {
                self.w_u8(tags::CREATE_ELEMENT);
                self.w_u64(node.raw());
                self.w_u16(tag.raw());
            }
            Op::CreateText { node, text } => {
                self.w_u8(tags::CREATE_TEXT);
                self.w_u64(node.raw());
                self.w_u32(text.raw());
            }
            Op::RemoveNode { node } => {
                self.w_u8(tags::REMOVE_NODE);
                self.w_u64(node.raw());
            }
            Op::InsertBefore {
                parent,
                child,
                anchor,
            } => {
                self.w_u8(tags::INSERT_BEFORE);
                self.w_u64(parent.raw());
                self.w_u64(child.raw());
                self.w_u64(anchor.raw());
            }
            Op::SetText { node, text } => {
                self.w_u8(tags::SET_TEXT);
                self.w_u64(node.raw());
                self.w_u32(text.raw());
            }
            Op::SetAttribute { node, attr, value } => {
                self.w_u8(tags::SET_ATTRIBUTE);
                self.w_u64(node.raw());
                self.w_u16(attr.raw());
                self.w_u32(value.raw());
            }
            Op::SetInlineStyle { node, prop, value } => {
                self.w_u8(tags::SET_INLINE_STYLE);
                self.w_u64(node.raw());
                self.w_u16(prop.raw());
                self.w_u32(value.raw());
            }
            Op::SetClass { node, class } => {
                self.w_u8(tags::SET_CLASS);
                self.w_u64(node.raw());
                self.w_u32(class.raw());
            }
            Op::RemoveClass { node, class } => {
                self.w_u8(tags::REMOVE_CLASS);
                self.w_u64(node.raw());
                self.w_u32(class.raw());
            }
            Op::SetTagName { node, name } => {
                self.w_u8(tags::SET_TAG_NAME);
                self.w_u64(node.raw());
                self.w_u32(name.raw());
            }
            Op::AddListener {
                node,
                event,
                handler,
            } => {
                self.w_u8(tags::ADD_LISTENER);
                self.w_u64(node.raw());
                self.w_u16(event.raw());
                self.w_u32(handler.raw());
            }
            Op::RemoveListener { node, event } => {
                self.w_u8(tags::REMOVE_LISTENER);
                self.w_u64(node.raw());
                self.w_u16(event.raw());
            }
            Op::InternString { id, bytes } => {
                self.w_u8(tags::INTERN_STRING);
                self.w_u32(id.raw());
                self.w_u32(bytes.len() as u32);
                self.buf.extend_from_slice(bytes);
            }
            Op::DispatchEvent {
                handler,
                node,
                payload,
            } => {
                self.w_u8(tags::DISPATCH_EVENT);
                self.w_u32(handler.raw());
                self.w_u64(node.raw());
                self.w_payload(payload);
            }
        }
    }

    /// Borrow the encoded bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Take the encoded bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Reset without freeing the backing allocation.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Number of bytes encoded so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been encoded yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn w_u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn w_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn w_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn w_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn w_f32(&mut self, v: f32) {
        self.w_u32(v.to_bits());
    }
    fn w_payload(&mut self, p: &EventPayload) {
        match p {
            EventPayload::None => self.w_u8(tags::PAYLOAD_NONE),
            EventPayload::Pointer { x, y, button } => {
                self.w_u8(tags::PAYLOAD_POINTER);
                self.w_f32(*x);
                self.w_f32(*y);
                self.w_u8(*button);
            }
            EventPayload::Key { code, mods } => {
                self.w_u8(tags::PAYLOAD_KEY);
                self.w_u32(*code);
                self.w_u8(*mods);
            }
            EventPayload::Text { text } => {
                self.w_u8(tags::PAYLOAD_TEXT);
                self.w_u32(text.raw());
            }
        }
    }
}

/// Anything that could go wrong decoding an op-stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Ran off the end of the buffer mid-op.
    Truncated,
    /// Encountered an unknown opcode or payload tag.
    UnknownTag(u8),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("op-stream truncated"),
            DecodeError::UnknownTag(t) => write!(f, "unknown op-stream tag {t:#04x}"),
        }
    }
}

/// Pull-decodes an op-stream produced by [`OpEncoder`].
pub struct OpReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> OpReader<'a> {
    /// New reader over an encoded buffer.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn r_u8(&mut self) -> Result<u8, DecodeError> {
        let b = *self.buf.get(self.pos).ok_or(DecodeError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }
    fn r_u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.r_array::<2>()?))
    }
    fn r_u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.r_array::<4>()?))
    }
    fn r_u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.r_array::<8>()?))
    }
    fn r_f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.r_u32()?))
    }
    fn r_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let end = self.pos.checked_add(N).ok_or(DecodeError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        self.pos = end;
        Ok(out)
    }
    fn r_bytes(&mut self, len: usize) -> Result<Vec<u8>, DecodeError> {
        let end = self.pos.checked_add(len).ok_or(DecodeError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        let out = slice.to_vec();
        self.pos = end;
        Ok(out)
    }

    fn r_payload(&mut self) -> Result<EventPayload, DecodeError> {
        match self.r_u8()? {
            tags::PAYLOAD_NONE => Ok(EventPayload::None),
            tags::PAYLOAD_POINTER => Ok(EventPayload::Pointer {
                x: self.r_f32()?,
                y: self.r_f32()?,
                button: self.r_u8()?,
            }),
            tags::PAYLOAD_KEY => Ok(EventPayload::Key {
                code: self.r_u32()?,
                mods: self.r_u8()?,
            }),
            tags::PAYLOAD_TEXT => Ok(EventPayload::Text {
                text: StrId::new(self.r_u32()?),
            }),
            other => Err(DecodeError::UnknownTag(other)),
        }
    }

    fn read_op(&mut self) -> Result<Op, DecodeError> {
        match self.r_u8()? {
            tags::BEGIN_BATCH => Ok(Op::BeginBatch {
                version: self.r_u16()?,
                seq: self.r_u32()?,
            }),
            tags::END_BATCH => Ok(Op::EndBatch),
            tags::CREATE_ELEMENT => Ok(Op::CreateElement {
                node: NodeId::new(self.r_u64()?),
                tag: ElementTag::new(self.r_u16()?),
            }),
            tags::CREATE_TEXT => Ok(Op::CreateText {
                node: NodeId::new(self.r_u64()?),
                text: StrId::new(self.r_u32()?),
            }),
            tags::REMOVE_NODE => Ok(Op::RemoveNode {
                node: NodeId::new(self.r_u64()?),
            }),
            tags::INSERT_BEFORE => Ok(Op::InsertBefore {
                parent: NodeId::new(self.r_u64()?),
                child: NodeId::new(self.r_u64()?),
                anchor: NodeId::new(self.r_u64()?),
            }),
            tags::SET_TEXT => Ok(Op::SetText {
                node: NodeId::new(self.r_u64()?),
                text: StrId::new(self.r_u32()?),
            }),
            tags::SET_ATTRIBUTE => Ok(Op::SetAttribute {
                node: NodeId::new(self.r_u64()?),
                attr: AttrId::new(self.r_u16()?),
                value: StrId::new(self.r_u32()?),
            }),
            tags::SET_INLINE_STYLE => Ok(Op::SetInlineStyle {
                node: NodeId::new(self.r_u64()?),
                prop: PropId::new(self.r_u16()?),
                value: StrId::new(self.r_u32()?),
            }),
            tags::SET_CLASS => Ok(Op::SetClass {
                node: NodeId::new(self.r_u64()?),
                class: StrId::new(self.r_u32()?),
            }),
            tags::REMOVE_CLASS => Ok(Op::RemoveClass {
                node: NodeId::new(self.r_u64()?),
                class: StrId::new(self.r_u32()?),
            }),
            tags::SET_TAG_NAME => Ok(Op::SetTagName {
                node: NodeId::new(self.r_u64()?),
                name: StrId::new(self.r_u32()?),
            }),
            tags::ADD_LISTENER => Ok(Op::AddListener {
                node: NodeId::new(self.r_u64()?),
                event: EventKind::new(self.r_u16()?),
                handler: HandlerId::new(self.r_u32()?),
            }),
            tags::REMOVE_LISTENER => Ok(Op::RemoveListener {
                node: NodeId::new(self.r_u64()?),
                event: EventKind::new(self.r_u16()?),
            }),
            tags::INTERN_STRING => {
                let id = StrId::new(self.r_u32()?);
                let len = self.r_u32()? as usize;
                let bytes = self.r_bytes(len)?;
                Ok(Op::InternString { id, bytes })
            }
            tags::DISPATCH_EVENT => Ok(Op::DispatchEvent {
                handler: HandlerId::new(self.r_u32()?),
                node: NodeId::new(self.r_u64()?),
                payload: self.r_payload()?,
            }),
            other => Err(DecodeError::UnknownTag(other)),
        }
    }
}

impl Iterator for OpReader<'_> {
    type Item = Result<Op, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let result = self.read_op();
        if result.is_err() {
            // Stop iterating after the first error.
            self.pos = self.buf.len();
        }
        Some(result)
    }
}

/// Decode an entire op-stream into a vector, or fail on the first bad op.
pub fn decode_all(buf: &[u8]) -> Result<Vec<Op>, DecodeError> {
    OpReader::new(buf).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn round_trips_a_mixed_batch() {
        let ops = vec![
            Op::BeginBatch {
                version: PROTOCOL_VERSION,
                seq: 7,
            },
            Op::InternString {
                id: StrId::new(0),
                bytes: b"Count: 0".to_vec(),
            },
            Op::CreateElement {
                node: NodeId::new(1),
                tag: ElementTag::new(3),
            },
            Op::CreateText {
                node: NodeId::new(2),
                text: StrId::new(0),
            },
            Op::InsertBefore {
                parent: NodeId::new(1),
                child: NodeId::new(2),
                anchor: NodeId::NULL,
            },
            Op::SetInlineStyle {
                node: NodeId::new(1),
                prop: PropId::new(5),
                value: StrId::new(0),
            },
            Op::AddListener {
                node: NodeId::new(1),
                event: EventKind::new(1),
                handler: HandlerId::new(42),
            },
            Op::DispatchEvent {
                handler: HandlerId::new(42),
                node: NodeId::new(1),
                payload: EventPayload::Pointer {
                    x: 120.0,
                    y: 48.5,
                    button: 0,
                },
            },
            Op::SetText {
                node: NodeId::new(2),
                text: StrId::new(0),
            },
            Op::EndBatch,
        ];

        let mut enc = OpEncoder::new();
        for op in &ops {
            enc.push(op);
        }
        let bytes = enc.into_bytes();

        assert_eq!(decode_all(&bytes).unwrap(), ops);
    }

    #[test]
    fn truncated_stream_is_an_error_not_a_panic() {
        let mut enc = OpEncoder::new();
        enc.push(&Op::CreateElement {
            node: NodeId::new(9),
            tag: ElementTag::new(1),
        });
        let bytes = enc.into_bytes();
        // Drop the last byte to truncate the operand.
        let truncated = &bytes[..bytes.len() - 1];
        assert_eq!(decode_all(truncated), Err(DecodeError::Truncated));
    }

    #[test]
    fn null_anchor_is_distinguishable() {
        assert!(NodeId::NULL.is_null());
        assert!(!NodeId::new(0).is_null());
    }
}
