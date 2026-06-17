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

#endif /* CANOPY_PROTOCOL_H */
