#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "canopy_displaylist.h" // CANOPY_DL_* tag bytes + CANOPY_DL_VERSION (the frozen wire contract)

// The "bring your own GPU renderer" seam.
//
// `canopy_host_build_display_list` (canopy.h) serializes one laid-out frame into the
// renderer-agnostic DISPLAY-LIST WIRE FORMAT documented byte-for-byte in canopy_displaylist.h:
// the geometric primitives (filled/rounded rects, borders, linear gradients, shadows, text runs,
// and the clip stack) a consumer decodes to issue its OWN draw calls — instead of taking the
// engine's software-rasterized pixels from canopy_host_render_rgba.
//
// This header is the C++ DECODER for that format. It is the dual of the Rust `deserialize` in
// crates/canopy-abi/src/displaylist.rs and is header-only and freestanding-safe: no exceptions,
// no RTTI, no heap, no std::variant — usable on the bare-metal frt tier as-is. The decoder is
// VISITOR-DRIVEN and ZERO-ALLOCATION: it streams the frame and calls duck-typed methods on a
// caller-supplied `sink`, handing each primitive's already-decoded fields straight through (text
// bytes are borrowed from the input buffer, never copied). On a truncated buffer or an unknown
// tag it returns `false` WITHOUT crashing and never reads past `len`.
//
// The `sink` is any object exposing these methods (all called in back-to-front paint order):
//   void rect(dl_rect bounds, dl_color color, float radius);
//   void glyphs(dl_color color, const dl_glyph* run, std::size_t glyph_count);
//   void text(dl_point origin, dl_color color, float size, float box_w, float align,
//             const char* text, std::size_t text_len);
//   void border(dl_rect bounds, dl_color color, float width, float radius);
//   void gradient(dl_rect bounds, std::uint8_t direction,
//                 const dl_gradient_stop* stops, std::size_t stop_count);
//   void shadow(dl_rect bounds, dl_color color, float blur, dl_point offset);
//   void push_clip(dl_rect bounds, float radius);
//   void pop_clip();
// `direction` is CANOPY_DL_DIR_VERTICAL or CANOPY_DL_DIR_HORIZONTAL. A renderer that does not
// implement a primitive may leave its method a no-op — a faithful degradation, exactly as the
// Rust renderers do.
namespace canopy::dl {

    // Plain-old-data geometry, mirroring the wire encoding (canopy_displaylist.h):
    // Color is 4 bytes r,g,b,a (straight alpha); Rect is 4 f32 x,y,w,h; Point is 2 f32 x,y.
    struct dl_color {
        std::uint8_t red;
        std::uint8_t green;
        std::uint8_t blue;
        std::uint8_t alpha;
    };

    struct dl_rect {
        float pos_x;
        float pos_y;
        float width;
        float height;
    };

    struct dl_point {
        float pos_x;
        float pos_y;
    };

    // One GRADIENT color stop: a color at a normalized position in [0, 1].
    struct dl_gradient_stop {
        dl_color color;
        float pos;
    };

    // One GLYPHS entry: a font glyph id placed at an absolute logical position.
    struct dl_glyph {
        std::uint32_t glyph_id;
        float pos_x;
        float pos_y;
    };

    namespace detail {

        // A bounds-checked little-endian cursor over the frame bytes. Every read is guarded: once
        // a read would pass the end, `ok_` latches false and all subsequent reads return zero, so
        // a truncated buffer degrades to a clean `false` from decode_display_list (never a crash,
        // never an out-of-bounds read). f32 is decoded from its LE IEEE-754 bit pattern.
        class byte_cursor {
        public:
            byte_cursor(const std::uint8_t* data, std::size_t len) : data_(data), len_(len) {}

            [[nodiscard]] bool ok() const {
                return ok_;
            }

            std::uint8_t read_u8() {
                if (pos_ + 1 > len_) {
                    ok_ = false;
                    return 0;
                }
                return data_[pos_++];
            }

            std::uint16_t read_u16() {
                if (pos_ + 2 > len_) {
                    ok_ = false;
                    return 0;
                }
                const std::uint16_t low_byte = data_[pos_];
                const std::uint16_t high_byte = data_[pos_ + 1];
                pos_ += 2;
                return static_cast<std::uint16_t>(low_byte | (high_byte << 8));
            }

            std::uint32_t read_u32() {
                if (pos_ + 4 > len_) {
                    ok_ = false;
                    return 0;
                }
                const std::uint32_t byte0 = data_[pos_];
                const std::uint32_t byte1 = data_[pos_ + 1];
                const std::uint32_t byte2 = data_[pos_ + 2];
                const std::uint32_t byte3 = data_[pos_ + 3];
                pos_ += 4;
                return byte0 | (byte1 << 8) | (byte2 << 16) | (byte3 << 24);
            }

            float read_f32() {
                const std::uint32_t bits = read_u32();
                float out = 0.0F;
                // Bit-cast the LE u32 into an IEEE-754 f32; both ends are trivially copyable
                // scalars. cpp-doctor: allow-next-line dangerous.no-memcpy-nontrivial-warning
                std::memcpy(&out, &bits, sizeof(out));
                return out;
            }

            dl_color read_color() {
                dl_color out{};
                out.red = read_u8();
                out.green = read_u8();
                out.blue = read_u8();
                out.alpha = read_u8();
                return out;
            }

            dl_rect read_rect() {
                dl_rect out{};
                out.pos_x = read_f32();
                out.pos_y = read_f32();
                out.width = read_f32();
                out.height = read_f32();
                return out;
            }

            dl_point read_point() {
                dl_point out{};
                out.pos_x = read_f32();
                out.pos_y = read_f32();
                return out;
            }

            // Borrow `count` raw bytes from the buffer (no copy); returns nullptr (and latches the
            // cursor not-ok) if fewer than `count` bytes remain.
            const std::uint8_t* take(std::size_t count) {
                if (pos_ + count > len_) {
                    ok_ = false;
                    return nullptr;
                }
                const std::uint8_t* start = data_ + pos_;
                pos_ += count;
                return start;
            }

        private:
            const std::uint8_t* data_;
            std::size_t len_;
            std::size_t pos_ = 0;
            bool ok_ = true;
        };

        // Decode a GLYPHS run's body (after the tag) and stream it to the sink in fixed windows, so
        // an arbitrarily long run never allocates. Returns false on truncation. A glyph_count of 0
        // still emits one empty run so the sink observes the item.
        template <class Sink> bool read_glyphs(byte_cursor& cursor, Sink& sink) {
            const dl_color color = cursor.read_color();
            const std::uint32_t glyph_count = cursor.read_u32();
            if (!cursor.ok()) {
                return false;
            }
            std::array<dl_glyph, 16> window{};
            std::size_t filled = 0;
            for (std::uint32_t idx = 0; idx < glyph_count; ++idx) {
                dl_glyph one{};
                one.glyph_id = cursor.read_u32();
                one.pos_x = cursor.read_f32();
                one.pos_y = cursor.read_f32();
                if (!cursor.ok()) {
                    return false;
                }
                // filled < window.size() here (it is reset on reaching the cap just below).
                // NOLINTNEXTLINE(cppcoreguidelines-pro-bounds-constant-array-index,cppcoreguidelines-pro-bounds-avoid-unchecked-container-access)
                window[filled++] = one;
                if (filled == window.size()) {
                    sink.glyphs(color, window.data(), filled);
                    filled = 0;
                }
            }
            if (filled > 0 || glyph_count == 0) {
                sink.glyphs(color, window.data(), filled);
            }
            return true;
        }

        // Decode a GRADIENT's body (after the tag) and hand its whole stop set to the sink in one
        // call. stop_count is a u8, so a fixed 256-entry buffer holds any valid run with no alloc.
        // Returns false on truncation.
        template <class Sink> bool read_gradient(byte_cursor& cursor, Sink& sink) {
            const dl_rect bounds = cursor.read_rect();
            const std::uint8_t direction = cursor.read_u8();
            const std::uint8_t stop_count = cursor.read_u8();
            if (!cursor.ok()) {
                return false;
            }
            std::array<dl_gradient_stop, 256> stops{};
            for (std::uint8_t idx = 0; idx < stop_count; ++idx) {
                dl_gradient_stop one{};
                one.color = cursor.read_color();
                one.pos = cursor.read_f32();
                if (!cursor.ok()) {
                    return false;
                }
                // idx < stop_count <= 255 < stops.size(), so the write is always in bounds.
                // NOLINTNEXTLINE(cppcoreguidelines-pro-bounds-constant-array-index,cppcoreguidelines-pro-bounds-avoid-unchecked-container-access)
                stops[idx] = one;
            }
            sink.gradient(bounds, direction, stops.data(), stop_count);
            return true;
        }

    } // namespace detail

    // Decode the display-list frame in `[data, data + len)` and drive `sink` per item, in paint
    // order. Returns true once the whole frame (version, dimensions, and every item) is consumed
    // cleanly; returns false on a truncated/malformed buffer or an unknown tag — in which case the
    // sink may have received the items decoded before the fault. Never reads past `len`; performs
    // no allocation. `Sink` is duck-typed (see the file header for the required method set).
    // The per-tag switch is a flat wire dispatcher; its size is inherent to the format, not a
    // structural problem, so the cognitive-complexity heuristic is suppressed here by design.
    template <class Sink>
    // NOLINTNEXTLINE(readability-function-cognitive-complexity)
    bool decode_display_list(const std::uint8_t* data, std::size_t len, Sink& sink) {
        if (data == nullptr) {
            return false;
        }
        detail::byte_cursor cursor(data, len);

        const std::uint16_t version = cursor.read_u16();
        const std::uint32_t frame_w = cursor.read_u32();
        const std::uint32_t frame_h = cursor.read_u32();
        const std::uint32_t count = cursor.read_u32();
        // the header fields are validated by the caller; we only need them consumed off the cursor
        static_cast<void>(version);
        static_cast<void>(frame_w);
        static_cast<void>(frame_h);
        if (!cursor.ok()) {
            return false;
        }

        for (std::uint32_t item = 0; item < count; ++item) {
            const std::uint8_t tag = cursor.read_u8();
            if (!cursor.ok()) {
                return false;
            }
            switch (tag) {
            case CANOPY_DL_RECT: {
                const dl_rect bounds = cursor.read_rect();
                const dl_color color = cursor.read_color();
                const float radius = cursor.read_f32();
                if (!cursor.ok()) {
                    return false;
                }
                sink.rect(bounds, color, radius);
                break;
            }
            case CANOPY_DL_GLYPHS: {
                if (!detail::read_glyphs(cursor, sink)) {
                    return false;
                }
                break;
            }
            case CANOPY_DL_TEXT: {
                const dl_point origin = cursor.read_point();
                const dl_color color = cursor.read_color();
                const float size = cursor.read_f32();
                const float box_w = cursor.read_f32();
                const float align = cursor.read_f32();
                const std::uint32_t text_len = cursor.read_u32();
                if (!cursor.ok()) {
                    return false;
                }
                const std::uint8_t* bytes = cursor.take(text_len);
                if (!cursor.ok()) {
                    return false;
                }
                // Hand the borrowed UTF-8 to the sink as char* (no copy); the bytes alias the input
                // buffer and outlive the call only as long as the caller keeps it.
                // NOLINTNEXTLINE(cppcoreguidelines-pro-type-reinterpret-cast)
                const char* chars = reinterpret_cast<const char*>(bytes);
                sink.text(origin, color, size, box_w, align, chars, text_len);
                break;
            }
            case CANOPY_DL_BORDER: {
                const dl_rect bounds = cursor.read_rect();
                const dl_color color = cursor.read_color();
                const float width = cursor.read_f32();
                const float radius = cursor.read_f32();
                if (!cursor.ok()) {
                    return false;
                }
                sink.border(bounds, color, width, radius);
                break;
            }
            case CANOPY_DL_GRADIENT: {
                if (!detail::read_gradient(cursor, sink)) {
                    return false;
                }
                break;
            }
            case CANOPY_DL_SHADOW: {
                const dl_rect bounds = cursor.read_rect();
                const dl_color color = cursor.read_color();
                const float blur = cursor.read_f32();
                const dl_point offset = cursor.read_point();
                if (!cursor.ok()) {
                    return false;
                }
                sink.shadow(bounds, color, blur, offset);
                break;
            }
            case CANOPY_DL_PUSH_CLIP: {
                const dl_rect bounds = cursor.read_rect();
                const float radius = cursor.read_f32();
                if (!cursor.ok()) {
                    return false;
                }
                sink.push_clip(bounds, radius);
                break;
            }
            case CANOPY_DL_POP_CLIP: {
                sink.pop_clip();
                break;
            }
            default:
                return false; // an unknown tag: stop rather than guess this item's field layout
            }
        }
        return true;
    }

    // Read just the frame header (version + viewport) without visiting any item. Returns false if
    // the 14-byte header does not fit. Handy for a consumer that wants to size its own surface or
    // assert the wire version before decoding the body.
    struct dl_header {
        std::uint16_t version;
        std::uint32_t width;
        std::uint32_t height;
        std::uint32_t count;
    };

    inline bool decode_header(const std::uint8_t* data, std::size_t len, dl_header& out) {
        if (data == nullptr) {
            return false;
        }
        detail::byte_cursor cursor(data, len);
        out.version = cursor.read_u16();
        out.width = cursor.read_u32();
        out.height = cursor.read_u32();
        out.count = cursor.read_u32();
        return cursor.ok();
    }

} // namespace canopy::dl
