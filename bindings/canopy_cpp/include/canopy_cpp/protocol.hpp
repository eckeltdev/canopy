#pragma once

#include <cstdint>

#include "canopy_protocol.h" // the machine-checked wire contract (crates/canopy-abi/include)

// Typed, namespaced views of the Canopy op-stream wire contract. The values come from
// `canopy_protocol.h`, which a Rust parity test pins to the engine's constants, so these
// can never drift from the protocol the host applies.
namespace canopy::wire {

    inline constexpr std::uint16_t protocol_version = CANOPY_PROTOCOL_VERSION;

    // Op tag bytes (guest -> host).
    inline constexpr std::uint8_t op_begin_batch = CANOPY_OP_BEGIN_BATCH;
    inline constexpr std::uint8_t op_end_batch = CANOPY_OP_END_BATCH;
    inline constexpr std::uint8_t op_create_element = CANOPY_OP_CREATE_ELEMENT;
    inline constexpr std::uint8_t op_create_text = CANOPY_OP_CREATE_TEXT;
    inline constexpr std::uint8_t op_remove_node = CANOPY_OP_REMOVE_NODE;
    inline constexpr std::uint8_t op_insert_before = CANOPY_OP_INSERT_BEFORE;
    inline constexpr std::uint8_t op_set_text = CANOPY_OP_SET_TEXT;
    inline constexpr std::uint8_t op_set_attribute = CANOPY_OP_SET_ATTRIBUTE;
    inline constexpr std::uint8_t op_set_inline_style = CANOPY_OP_SET_INLINE_STYLE;
    inline constexpr std::uint8_t op_set_class = CANOPY_OP_SET_CLASS;
    inline constexpr std::uint8_t op_remove_class = CANOPY_OP_REMOVE_CLASS;
    inline constexpr std::uint8_t op_add_listener = CANOPY_OP_ADD_LISTENER;
    inline constexpr std::uint8_t op_remove_listener = CANOPY_OP_REMOVE_LISTENER;
    inline constexpr std::uint8_t op_intern_string = CANOPY_OP_INTERN_STRING;
    inline constexpr std::uint8_t op_set_tag_name = CANOPY_OP_SET_TAG_NAME;

    // Reserved handles.
    inline constexpr std::uint64_t node_root = CANOPY_NODE_ROOT;
    inline constexpr std::uint64_t node_null = CANOPY_NODE_NULL;
    inline constexpr std::uint16_t attr_id = CANOPY_ATTR_ID;

    // Host-tier widget / event / property ids (canopy-view / canopy-paint convention).
    inline constexpr std::uint16_t el_column = CANOPY_EL_COLUMN;
    inline constexpr std::uint16_t el_row = CANOPY_EL_ROW;
    inline constexpr std::uint16_t el_button = CANOPY_EL_BUTTON;
    inline constexpr std::uint16_t el_input = CANOPY_EL_INPUT;
    inline constexpr std::uint16_t event_click = CANOPY_EVENT_CLICK;
    inline constexpr std::uint16_t prop_bg = CANOPY_PROP_BG;
    inline constexpr std::uint16_t prop_fg = CANOPY_PROP_FG;
    inline constexpr std::uint16_t prop_width = CANOPY_PROP_WIDTH;
    inline constexpr std::uint16_t prop_height = CANOPY_PROP_HEIGHT;
    inline constexpr std::uint16_t prop_padding = CANOPY_PROP_PADDING;

} // namespace canopy::wire
