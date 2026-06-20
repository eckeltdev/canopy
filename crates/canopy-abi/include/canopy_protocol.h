/*
 * canopy_protocol.h — the Canopy op-stream WIRE CONTRACT for non-Rust authors.
 *
 * `canopy.h` declares the C ABI functions (create/apply/free). THIS header is the
 * other half: the byte-level op protocol an author in another language (a C++ builder
 * DSL, a Python binding, …) must reproduce to emit batches that `canopy_host_apply`
 * accepts. It is the contract of record for the encoder.
 *
 * Encoding rules
 * --------------
 *   * Every op is one tag byte, then its fields in declaration order.
 *   * All multi-byte integers are LITTLE-ENDIAN.
 *   * `f32` is its IEEE-754 bit pattern written as a little-endian u32.
 *   * Strings are UTF-8, no NUL terminator, interned once (see InternString).
 *   * A batch is:  BeginBatch(version, seq)  op*  EndBatch  — no length/magic/checksum
 *     inside the stream; the outer frame is the (ptr,len) pair to canopy_host_apply.
 *
 * Handle widths (little-endian):
 *   NodeId    u64      HandlerId u32      AttrId  u16
 *   StrId     u32      ElementTag u16     PropId  u16
 *   EventKind u16
 *
 * Stateful emitter rules an encoder MUST follow (see canopy-core::Emitter):
 *   * NodeId is author-minted, monotonic from 1 (0 is the reserved ROOT).
 *   * HandlerId is author-minted, monotonic from 0.
 *   * Intern each unique string exactly once (one InternString op); reuse its StrId.
 *   * The intern table and the node counter PERSIST across batches.
 *
 * The "PROTOCOL" section below is the stable wire contract. The "HOST-TIER IDS"
 * section is the *convention* of the reference host (canopy-view / canopy-paint):
 * ElementTag / EventKind / PropId values are assigned by the host registry, not the
 * protocol — an author must use the same numbers as the host build it targets.
 *
 * Machine-checked: crates/canopy-abi/tests/protocol_header.rs parses this file and
 * asserts every value below equals the corresponding Rust constant, so the header
 * cannot drift from the engine.
 */

#ifndef CANOPY_PROTOCOL_H
#define CANOPY_PROTOCOL_H

#include <stdint.h>

/* ---- PROTOCOL: stable wire contract (canopy-protocol) ----------------------- */

#define CANOPY_PROTOCOL_VERSION 1u            /* written into BeginBatch.version  */
#define CANOPY_MAX_BATCH_BYTES  (1u << 20)    /* 1 MiB; canopy_host_apply rejects more */
#define CANOPY_MAX_EVENT_BATCH_BYTES (64u << 10) /* 64 KiB; an out buffer this big always
                                                    drains canopy_host_poll_events at once */

#define CANOPY_NODE_ROOT 0ull                 /* the implicit mount parent        */
#define CANOPY_NODE_NULL 0xFFFFFFFFFFFFFFFFull /* InsertBefore.anchor = append    */
#define CANOPY_ATTR_ID   1u                   /* the one reserved AttrId (CSS id) */

/* Op tag bytes (guest -> host). */
#define CANOPY_OP_BEGIN_BATCH      0x01u
#define CANOPY_OP_END_BATCH        0x02u
#define CANOPY_OP_CREATE_ELEMENT   0x10u
#define CANOPY_OP_CREATE_TEXT      0x11u
#define CANOPY_OP_REMOVE_NODE      0x12u
#define CANOPY_OP_INSERT_BEFORE    0x13u
#define CANOPY_OP_SET_TEXT         0x14u
#define CANOPY_OP_SET_ATTRIBUTE    0x15u
#define CANOPY_OP_SET_INLINE_STYLE 0x16u
#define CANOPY_OP_SET_CLASS        0x17u
#define CANOPY_OP_REMOVE_CLASS     0x18u
#define CANOPY_OP_ADD_LISTENER     0x19u
#define CANOPY_OP_REMOVE_LISTENER  0x1Au
#define CANOPY_OP_INTERN_STRING    0x1Bu
#define CANOPY_OP_SET_TAG_NAME     0x1Cu

/* Op tag byte (host -> guest) — the only inbound op an author DECODES, for events.
 * NOTE the field order: DispatchEvent writes handler(u32) BEFORE node(u64). */
#define CANOPY_OP_DISPATCH_EVENT   0x80u

/* EventPayload sub-tags (1 byte), inside DispatchEvent. */
#define CANOPY_PAYLOAD_NONE    0u
#define CANOPY_PAYLOAD_POINTER 1u   /* x:f32, y:f32, button:u8 */
#define CANOPY_PAYLOAD_KEY     2u   /* code:u32, mods:u8       */
#define CANOPY_PAYLOAD_TEXT    3u   /* text:StrId(u32)         */

/* ---- HOST-TIER IDS: reference-host convention (canopy-view / canopy-paint) ---
 * NOT part of the protocol — these are the numbers the canopy-view/canopy-paint
 * host assigns. An author emits these so the host interprets them correctly. A
 * different host could choose different numbers. */

/* ElementTag (CreateElement.tag). */
#define CANOPY_EL_COLUMN 1u   /* a flex/block container (direction from CSS) */
#define CANOPY_EL_ROW    2u
#define CANOPY_EL_BUTTON 3u
#define CANOPY_EL_INPUT  4u

/* EventKind (AddListener.event). */
#define CANOPY_EVENT_CLICK 1u

/* PropId (SetInlineStyle.prop). */
#define CANOPY_PROP_BG          1u
#define CANOPY_PROP_FG          2u
#define CANOPY_PROP_WIDTH       3u
#define CANOPY_PROP_HEIGHT      4u
#define CANOPY_PROP_GAP         5u
#define CANOPY_PROP_PADDING     6u
#define CANOPY_PROP_DIRECTION   7u
#define CANOPY_PROP_RADIUS      8u
#define CANOPY_PROP_OPACITY     9u
#define CANOPY_PROP_TRANSLATE_X 10u
#define CANOPY_PROP_TRANSLATE_Y 11u
#define CANOPY_PROP_ALIGN       12u
#define CANOPY_PROP_JUSTIFY     13u
#define CANOPY_PROP_TEXT_ALIGN  14u
#define CANOPY_PROP_MARGIN       15u
#define CANOPY_PROP_MIN_WIDTH    16u
#define CANOPY_PROP_MIN_HEIGHT   17u
#define CANOPY_PROP_MAX_WIDTH    18u
#define CANOPY_PROP_MAX_HEIGHT   19u
#define CANOPY_PROP_FLEX_GROW    20u
#define CANOPY_PROP_BORDER_WIDTH 21u
#define CANOPY_PROP_BORDER_COLOR 22u
/* Box model — per-side margin/padding. */
#define CANOPY_PROP_MARGIN_TOP     23u
#define CANOPY_PROP_MARGIN_RIGHT   24u
#define CANOPY_PROP_MARGIN_BOTTOM  25u
#define CANOPY_PROP_MARGIN_LEFT    26u
#define CANOPY_PROP_PADDING_TOP    27u
#define CANOPY_PROP_PADDING_RIGHT  28u
#define CANOPY_PROP_PADDING_BOTTOM 29u
#define CANOPY_PROP_PADDING_LEFT   30u
/* Display / visibility. */
#define CANOPY_PROP_DISPLAY    31u
#define CANOPY_PROP_VISIBILITY 32u
/* Position. */
#define CANOPY_PROP_POSITION     33u
#define CANOPY_PROP_INSET_TOP    34u
#define CANOPY_PROP_INSET_RIGHT  35u
#define CANOPY_PROP_INSET_BOTTOM 36u
#define CANOPY_PROP_INSET_LEFT   37u
#define CANOPY_PROP_Z_INDEX      38u
/* Flex. */
#define CANOPY_PROP_FLEX_WRAP   39u
#define CANOPY_PROP_FLEX_BASIS  40u
#define CANOPY_PROP_FLEX_SHRINK 41u
#define CANOPY_PROP_ALIGN_SELF  42u
/* Sizing. */
#define CANOPY_PROP_ASPECT_RATIO 43u
#define CANOPY_PROP_BOX_SIZING   44u
/* Gaps — per-axis. */
#define CANOPY_PROP_ROW_GAP    45u
#define CANOPY_PROP_COLUMN_GAP 46u
/* Overflow — reserved for a later wave. */
#define CANOPY_PROP_OVERFLOW 47u
/* Border longhands. */
#define CANOPY_PROP_BORDER_STYLE              48u
#define CANOPY_PROP_BORDER_TOP_WIDTH          49u
#define CANOPY_PROP_BORDER_RIGHT_WIDTH        50u
#define CANOPY_PROP_BORDER_BOTTOM_WIDTH       51u
#define CANOPY_PROP_BORDER_LEFT_WIDTH         52u
#define CANOPY_PROP_BORDER_TOP_COLOR          53u
#define CANOPY_PROP_BORDER_RIGHT_COLOR        54u
#define CANOPY_PROP_BORDER_BOTTOM_COLOR       55u
#define CANOPY_PROP_BORDER_LEFT_COLOR         56u
#define CANOPY_PROP_BORDER_TOP_LEFT_RADIUS    57u
#define CANOPY_PROP_BORDER_TOP_RIGHT_RADIUS   58u
#define CANOPY_PROP_BORDER_BOTTOM_RIGHT_RADIUS 59u
#define CANOPY_PROP_BORDER_BOTTOM_LEFT_RADIUS  60u
/* Text. */
#define CANOPY_PROP_FONT_SIZE       61u
#define CANOPY_PROP_FONT_WEIGHT     62u
#define CANOPY_PROP_LINE_HEIGHT     63u
#define CANOPY_PROP_TEXT_DECORATION 64u
/* Outline. */
#define CANOPY_PROP_OUTLINE_WIDTH  65u
#define CANOPY_PROP_OUTLINE_COLOR  66u
#define CANOPY_PROP_OUTLINE_OFFSET 67u
/* Effects. */
#define CANOPY_PROP_BOX_SHADOW       68u
#define CANOPY_PROP_BACKGROUND_IMAGE 69u
/* Grid (CSS Grid for the lite tier). */
#define CANOPY_PROP_GRID_TEMPLATE_COLUMNS 70u
#define CANOPY_PROP_GRID_TEMPLATE_ROWS    71u
#define CANOPY_PROP_GRID_COLUMN           72u
#define CANOPY_PROP_GRID_ROW              73u
#define CANOPY_PROP_GRID_AUTO_FLOW        74u
#define CANOPY_PROP_JUSTIFY_ITEMS         75u

#endif /* CANOPY_PROTOCOL_H */
