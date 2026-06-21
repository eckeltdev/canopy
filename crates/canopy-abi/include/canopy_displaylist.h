/*
 * canopy_displaylist.h — the Canopy DISPLAY-LIST WIRE FORMAT for non-Rust renderers.
 *
 * `canopy_protocol.h` is the op-stream a guest emits to BUILD the tree (guest -> host).
 * THIS header is the dual: the byte format `canopy_host_build_display_list` (in canopy.h)
 * produces, host -> consumer, describing one laid-out frame as a flat list of geometric
 * primitives. A consumer decodes it and issues its OWN GPU / 2D-accelerator draw calls —
 * the "bring your own renderer" path — instead of taking canopy_host_render_rgba's
 * software-rasterized pixels.
 *
 * Encoding rules (identical conventions to the op-stream):
 *   * All multi-byte integers are LITTLE-ENDIAN.
 *   * `f32` is its IEEE-754 bit pattern written as a little-endian u32.
 *   * `Color` is 4 bytes: r, g, b, a (straight alpha).
 *   * `Rect` is 4 f32: x, y, w, h (absolute logical coords, top-left origin).
 *   * `Point` is 2 f32: x, y.
 *   * Strings are UTF-8, length-prefixed (u32), no NUL terminator.
 *
 * Frame:
 *   version:u16  width:u32  height:u32  count:u32   item * count
 *
 * Each item is a 1-byte tag (CANOPY_DL_* below) then its fields IN THIS ORDER:
 *   RECT      rect       color  radius:f32
 *   GLYPHS    color      glyph_count:u32   (id:u32 x:f32 y:f32) * glyph_count
 *   TEXT      origin     color  size:f32 box_w:f32 align:f32  text_len:u32  utf8[text_len]
 *   BORDER    rect       color  width:f32 radius:f32
 *   GRADIENT  rect       direction:u8 (0=vertical,1=horizontal)  stop_count:u8
 *                        (color pos:f32) * stop_count
 *   SHADOW    rect       color  blur:f32  offset:Point
 *   PUSH_CLIP rect       radius:f32       (mask following items to this rounded rect)
 *   POP_CLIP  (no fields)                 (restore the enclosing clip)
 *
 * Paint order is the list order, back-to-front. A renderer that does not implement a
 * primitive (e.g. soft shadows, or clipping) may skip that item and its PopClip — a faithful
 * degradation, exactly as the Rust renderers do. Text arrives as either a baked-font TEXT run
 * (the lite/device tier — you rasterize it) or a pre-shaped GLYPHS run (the capable tier).
 *
 * Machine-checked: crates/canopy-abi/tests/displaylist_header.rs asserts every value below
 * equals the corresponding `canopy_abi::displaylist::DL_*` constant, so the header cannot drift.
 */

#ifndef CANOPY_DISPLAYLIST_H
#define CANOPY_DISPLAYLIST_H

#include <stdint.h>

#define CANOPY_DL_VERSION 1u /* written into the frame header `version` field */

/* Item tag bytes (host -> consumer). */
#define CANOPY_DL_RECT      0x01u
#define CANOPY_DL_GLYPHS    0x02u
#define CANOPY_DL_TEXT      0x03u
#define CANOPY_DL_BORDER    0x04u
#define CANOPY_DL_GRADIENT  0x05u
#define CANOPY_DL_SHADOW    0x06u
#define CANOPY_DL_PUSH_CLIP 0x07u
#define CANOPY_DL_POP_CLIP  0x08u

/* GRADIENT.direction values. */
#define CANOPY_DL_DIR_VERTICAL   0u
#define CANOPY_DL_DIR_HORIZONTAL 1u

#endif /* CANOPY_DISPLAYLIST_H */
