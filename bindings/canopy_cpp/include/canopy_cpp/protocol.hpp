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
    // The one host -> guest op (echoed back through poll_events); note it writes handler(u32)
    // BEFORE node(u64), opposite of every guest -> host op.
    inline constexpr std::uint8_t op_dispatch_event = CANOPY_OP_DISPATCH_EVENT;

    // DispatchEvent payload sub-tags.
    inline constexpr std::uint8_t payload_none = CANOPY_PAYLOAD_NONE;
    inline constexpr std::uint8_t payload_pointer =
        CANOPY_PAYLOAD_POINTER;                                       // x:f32, y:f32, button:u8
    inline constexpr std::uint8_t payload_key = CANOPY_PAYLOAD_KEY;   // code:u32, mods:u8
    inline constexpr std::uint8_t payload_text = CANOPY_PAYLOAD_TEXT; // text:StrId(u32)

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
    inline constexpr std::uint16_t prop_gap = CANOPY_PROP_GAP;
    inline constexpr std::uint16_t prop_padding = CANOPY_PROP_PADDING;
    inline constexpr std::uint16_t prop_direction = CANOPY_PROP_DIRECTION;
    inline constexpr std::uint16_t prop_radius = CANOPY_PROP_RADIUS;
    inline constexpr std::uint16_t prop_opacity = CANOPY_PROP_OPACITY;
    inline constexpr std::uint16_t prop_translate_x = CANOPY_PROP_TRANSLATE_X;
    inline constexpr std::uint16_t prop_translate_y = CANOPY_PROP_TRANSLATE_Y;
    inline constexpr std::uint16_t prop_align = CANOPY_PROP_ALIGN;
    inline constexpr std::uint16_t prop_justify = CANOPY_PROP_JUSTIFY;
    inline constexpr std::uint16_t prop_text_align = CANOPY_PROP_TEXT_ALIGN;
    inline constexpr std::uint16_t prop_margin = CANOPY_PROP_MARGIN;
    inline constexpr std::uint16_t prop_min_width = CANOPY_PROP_MIN_WIDTH;
    inline constexpr std::uint16_t prop_min_height = CANOPY_PROP_MIN_HEIGHT;
    inline constexpr std::uint16_t prop_max_width = CANOPY_PROP_MAX_WIDTH;
    inline constexpr std::uint16_t prop_max_height = CANOPY_PROP_MAX_HEIGHT;
    inline constexpr std::uint16_t prop_flex_grow = CANOPY_PROP_FLEX_GROW;
    inline constexpr std::uint16_t prop_border_width = CANOPY_PROP_BORDER_WIDTH;
    inline constexpr std::uint16_t prop_border_color = CANOPY_PROP_BORDER_COLOR;
    // Box model — per-side margin/padding.
    inline constexpr std::uint16_t prop_margin_top = CANOPY_PROP_MARGIN_TOP;
    inline constexpr std::uint16_t prop_margin_right = CANOPY_PROP_MARGIN_RIGHT;
    inline constexpr std::uint16_t prop_margin_bottom = CANOPY_PROP_MARGIN_BOTTOM;
    inline constexpr std::uint16_t prop_margin_left = CANOPY_PROP_MARGIN_LEFT;
    inline constexpr std::uint16_t prop_padding_top = CANOPY_PROP_PADDING_TOP;
    inline constexpr std::uint16_t prop_padding_right = CANOPY_PROP_PADDING_RIGHT;
    inline constexpr std::uint16_t prop_padding_bottom = CANOPY_PROP_PADDING_BOTTOM;
    inline constexpr std::uint16_t prop_padding_left = CANOPY_PROP_PADDING_LEFT;
    // Display / visibility.
    inline constexpr std::uint16_t prop_display = CANOPY_PROP_DISPLAY;
    inline constexpr std::uint16_t prop_visibility = CANOPY_PROP_VISIBILITY;
    // Position.
    inline constexpr std::uint16_t prop_position = CANOPY_PROP_POSITION;
    inline constexpr std::uint16_t prop_inset_top = CANOPY_PROP_INSET_TOP;
    inline constexpr std::uint16_t prop_inset_right = CANOPY_PROP_INSET_RIGHT;
    inline constexpr std::uint16_t prop_inset_bottom = CANOPY_PROP_INSET_BOTTOM;
    inline constexpr std::uint16_t prop_inset_left = CANOPY_PROP_INSET_LEFT;
    inline constexpr std::uint16_t prop_z_index = CANOPY_PROP_Z_INDEX;
    // Flex.
    inline constexpr std::uint16_t prop_flex_wrap = CANOPY_PROP_FLEX_WRAP;
    inline constexpr std::uint16_t prop_flex_basis = CANOPY_PROP_FLEX_BASIS;
    inline constexpr std::uint16_t prop_flex_shrink = CANOPY_PROP_FLEX_SHRINK;
    inline constexpr std::uint16_t prop_align_self = CANOPY_PROP_ALIGN_SELF;
    // Sizing.
    inline constexpr std::uint16_t prop_aspect_ratio = CANOPY_PROP_ASPECT_RATIO;
    inline constexpr std::uint16_t prop_box_sizing = CANOPY_PROP_BOX_SIZING;
    // Gaps — per-axis.
    inline constexpr std::uint16_t prop_row_gap = CANOPY_PROP_ROW_GAP;
    inline constexpr std::uint16_t prop_column_gap = CANOPY_PROP_COLUMN_GAP;
    // Overflow — reserved for a later wave.
    inline constexpr std::uint16_t prop_overflow = CANOPY_PROP_OVERFLOW;
    // Border longhands.
    inline constexpr std::uint16_t prop_border_style = CANOPY_PROP_BORDER_STYLE;
    inline constexpr std::uint16_t prop_border_top_width = CANOPY_PROP_BORDER_TOP_WIDTH;
    inline constexpr std::uint16_t prop_border_right_width = CANOPY_PROP_BORDER_RIGHT_WIDTH;
    inline constexpr std::uint16_t prop_border_bottom_width = CANOPY_PROP_BORDER_BOTTOM_WIDTH;
    inline constexpr std::uint16_t prop_border_left_width = CANOPY_PROP_BORDER_LEFT_WIDTH;
    inline constexpr std::uint16_t prop_border_top_color = CANOPY_PROP_BORDER_TOP_COLOR;
    inline constexpr std::uint16_t prop_border_right_color = CANOPY_PROP_BORDER_RIGHT_COLOR;
    inline constexpr std::uint16_t prop_border_bottom_color = CANOPY_PROP_BORDER_BOTTOM_COLOR;
    inline constexpr std::uint16_t prop_border_left_color = CANOPY_PROP_BORDER_LEFT_COLOR;
    inline constexpr std::uint16_t prop_border_top_left_radius = CANOPY_PROP_BORDER_TOP_LEFT_RADIUS;
    inline constexpr std::uint16_t prop_border_top_right_radius = CANOPY_PROP_BORDER_TOP_RIGHT_RADIUS;
    inline constexpr std::uint16_t prop_border_bottom_right_radius =
        CANOPY_PROP_BORDER_BOTTOM_RIGHT_RADIUS;
    inline constexpr std::uint16_t prop_border_bottom_left_radius =
        CANOPY_PROP_BORDER_BOTTOM_LEFT_RADIUS;
    // Text.
    inline constexpr std::uint16_t prop_font_size = CANOPY_PROP_FONT_SIZE;
    inline constexpr std::uint16_t prop_font_weight = CANOPY_PROP_FONT_WEIGHT;
    inline constexpr std::uint16_t prop_line_height = CANOPY_PROP_LINE_HEIGHT;
    inline constexpr std::uint16_t prop_text_decoration = CANOPY_PROP_TEXT_DECORATION;
    // Outline.
    inline constexpr std::uint16_t prop_outline_width = CANOPY_PROP_OUTLINE_WIDTH;
    inline constexpr std::uint16_t prop_outline_color = CANOPY_PROP_OUTLINE_COLOR;
    inline constexpr std::uint16_t prop_outline_offset = CANOPY_PROP_OUTLINE_OFFSET;
    // Effects.
    inline constexpr std::uint16_t prop_box_shadow = CANOPY_PROP_BOX_SHADOW;
    inline constexpr std::uint16_t prop_background_image = CANOPY_PROP_BACKGROUND_IMAGE;
    // Grid (CSS Grid for the lite tier).
    inline constexpr std::uint16_t prop_grid_template_columns = CANOPY_PROP_GRID_TEMPLATE_COLUMNS;
    inline constexpr std::uint16_t prop_grid_template_rows = CANOPY_PROP_GRID_TEMPLATE_ROWS;
    inline constexpr std::uint16_t prop_grid_column = CANOPY_PROP_GRID_COLUMN;
    inline constexpr std::uint16_t prop_grid_row = CANOPY_PROP_GRID_ROW;
    inline constexpr std::uint16_t prop_grid_auto_flow = CANOPY_PROP_GRID_AUTO_FLOW;
    inline constexpr std::uint16_t prop_justify_items = CANOPY_PROP_JUSTIFY_ITEMS;

} // namespace canopy::wire
