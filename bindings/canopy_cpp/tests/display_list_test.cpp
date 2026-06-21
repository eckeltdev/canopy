#include <cstddef>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#include "canopy.h"             // the Rust C ABI: canopy_host_* + CANOPY_OK / CANOPY_ERR_TOO_LARGE
#include "canopy_displaylist.h" // CANOPY_DL_VERSION (asserted against the decoded frame header)

#include "canopy_cpp/build_context.hpp" // canopy::build_context (the op-batch the DSL fills)
#include "canopy_cpp/display_list.hpp"  // the decoder under test
#include "canopy_cpp/dsl.hpp"           // build a real styled tree to feed the producer

// Exercises the C++ display-list DECODER (display_list.hpp) end to end against the REAL engine:
// author an identity-only tree with the DSL, install a stylesheet whose rules produce rich
// primitives (linear-gradient background, box-shadow, border, rounded radius, and a text child),
// apply it, then call canopy_host_build_display_list twice (size, then fill) and decode the frame
// with a counting sink. Asserts the frame header and that a gradient, a shadow, a border, and a
// text item were all visited; and that the decoder rejects a truncated buffer. Links
// libcanopy_abi.a (build it first: `cargo build -p canopy-abi`). Plain executable — prints "ok:
// ..." or returns non-zero, no test framework.
namespace {

    constexpr std::uint32_t view_w = 400;
    constexpr std::uint32_t view_h = 300;

    // A stylesheet whose matched declarations make the lite engine emit each rich primitive: a
    // gradient (background-image), a shadow (box-shadow), a border (border-width/-color), a rounded
    // corner (radius), and inherited text traits for the text child.
    constexpr const char* stylesheet =
        "#root { width: 400; height: 300; padding: 24; direction: column;"
        "        background-image: linear-gradient(to bottom, #1e1e2e, #11111b) }"
        "#card { width: 320; height: 200; radius: 18; padding: 20; direction: column;"
        "        background: #313244; box-shadow: 0 10 28 #00000088;"
        "        border-width: 3; border-color: #89b4fa }"
        ".label { font-size: 22; font-weight: bold; text-align: center; color: #cdd6f4 }";

    // A sink that just counts each primitive kind it is handed (the decoder's full method set).
    struct counting_sink {
        std::size_t rects = 0;
        std::size_t glyph_runs = 0;
        std::size_t texts = 0;
        std::size_t borders = 0;
        std::size_t gradients = 0;
        std::size_t shadows = 0;
        std::size_t pushes = 0;
        std::size_t pops = 0;
        std::size_t text_bytes_seen = 0;

        void rect(canopy::dl::dl_rect /*bounds*/, canopy::dl::dl_color /*color*/,
                  float /*radius*/) {
            ++rects;
        }
        void glyphs(canopy::dl::dl_color /*color*/, const canopy::dl::dl_glyph* /*run*/,
                    std::size_t /*glyph_count*/) {
            ++glyph_runs;
        }
        void text(canopy::dl::dl_point /*origin*/, canopy::dl::dl_color /*color*/, float /*size*/,
                  float /*box_w*/, float /*align*/, const char* /*chars*/, std::size_t text_len) {
            ++texts;
            text_bytes_seen += text_len;
        }
        void border(canopy::dl::dl_rect /*bounds*/, canopy::dl::dl_color /*color*/, float /*width*/,
                    float /*radius*/) {
            ++borders;
        }
        void gradient(canopy::dl::dl_rect /*bounds*/, std::uint8_t /*direction*/,
                      const canopy::dl::dl_gradient_stop* /*stops*/, std::size_t /*stop_count*/) {
            ++gradients;
        }
        void shadow(canopy::dl::dl_rect /*bounds*/, canopy::dl::dl_color /*color*/, float /*blur*/,
                    canopy::dl::dl_point /*offset*/) {
            ++shadows;
        }
        void push_clip(canopy::dl::dl_rect /*bounds*/, float /*radius*/) {
            ++pushes;
        }
        void pop_clip() {
            ++pops;
        }
    };

    // Build the identity-only tree, install the stylesheet, apply, and serialize one display-list
    // frame for view_w x view_h via the two-call needed-size contract. Returns the wire bytes (or
    // empty on any FFI error).
    std::vector<std::uint8_t> build_frame() {
        canopy::build_context ctx;
        {
            using namespace canopy; // DSL factories
            mount(ctx, div(id("root"), div(id("card"), div(cls("label"), text("Canopy")))));
        }

        CanopyHost* host = canopy_host_new();
        const std::vector<std::uint8_t> sheet(stylesheet,
                                              stylesheet + std::string(stylesheet).size());
        if (canopy_host_set_stylesheet(host, sheet.data(), sheet.size()) != CANOPY_OK) {
            std::cerr << "FAIL: canopy_host_set_stylesheet\n";
            canopy_host_free(host);
            return {};
        }
        const std::vector<std::uint8_t> batch = ctx.take_batch(0);
        if (canopy_host_apply(host, batch.data(), batch.size()) != CANOPY_OK) {
            std::cerr << "FAIL: canopy_host_apply\n";
            canopy_host_free(host);
            return {};
        }

        // Call 1: size (cap = 0 -> needed length in *out_len, nothing written).
        std::size_t needed = 0;
        const int size_rc =
            canopy_host_build_display_list(host, view_w, view_h, nullptr, 0, &needed);
        if (size_rc != CANOPY_ERR_TOO_LARGE || needed == 0) {
            std::cerr << "FAIL: sizing call returned " << size_rc << " needed=" << needed << '\n';
            canopy_host_free(host);
            return {};
        }

        // Call 2: fill the allocated buffer.
        std::vector<std::uint8_t> frame(needed);
        std::size_t written = 0;
        const int fill_rc = canopy_host_build_display_list(host, view_w, view_h, frame.data(),
                                                           frame.size(), &written);
        canopy_host_free(host);
        if (fill_rc != CANOPY_OK || written != needed) {
            std::cerr << "FAIL: fill call returned " << fill_rc << " written=" << written << '\n';
            return {};
        }
        frame.resize(written);
        return frame;
    }

    bool decodes_a_rich_frame() {
        const std::vector<std::uint8_t> frame = build_frame();
        if (frame.empty()) {
            return false;
        }

        // The decoded header must report the frozen version and the exact viewport we built for.
        canopy::dl::dl_header header{};
        if (!canopy::dl::decode_header(frame.data(), frame.size(), header)) {
            std::cerr << "FAIL: decode_header on a valid frame\n";
            return false;
        }
        if (header.version != CANOPY_DL_VERSION) {
            std::cerr << "FAIL: version " << header.version << " != " << CANOPY_DL_VERSION << '\n';
            return false;
        }
        if (header.width != view_w || header.height != view_h) {
            std::cerr << "FAIL: frame size " << header.width << 'x' << header.height
                      << " != " << view_w << 'x' << view_h << '\n';
            return false;
        }

        counting_sink sink;
        const bool decoded = canopy::dl::decode_display_list(frame.data(), frame.size(), sink);
        if (!decoded) {
            std::cerr << "FAIL: decode_display_list returned false on a valid frame\n";
            return false;
        }

        if (sink.gradients == 0) {
            std::cerr << "FAIL: expected at least one gradient\n";
            return false;
        }
        if (sink.shadows == 0) {
            std::cerr << "FAIL: expected at least one shadow\n";
            return false;
        }
        if (sink.borders == 0) {
            std::cerr << "FAIL: expected at least one border\n";
            return false;
        }
        if (sink.texts == 0 || sink.text_bytes_seen == 0) {
            std::cerr << "FAIL: expected at least one (non-empty) text run\n";
            return false;
        }

        std::cerr << "ok: decoded frame " << header.width << 'x' << header.height << " v"
                  << header.version << " — rects=" << sink.rects << " gradients=" << sink.gradients
                  << " shadows=" << sink.shadows << " borders=" << sink.borders
                  << " texts=" << sink.texts << " glyph_runs=" << sink.glyph_runs
                  << " push/pop=" << sink.pushes << '/' << sink.pops << '\n';
        return true;
    }

    // A buffer one byte short of the full frame must decode to `false`, without crashing or reading
    // past the end (the truncation guard in the byte cursor).
    bool rejects_a_truncated_frame() {
        std::vector<std::uint8_t> frame = build_frame();
        if (frame.size() < 2) {
            std::cerr << "FAIL: frame too small to truncate\n";
            return false;
        }
        frame.pop_back(); // drop the final byte: the last item is now incomplete

        counting_sink sink;
        const bool decoded = canopy::dl::decode_display_list(frame.data(), frame.size(), sink);
        if (decoded) {
            std::cerr << "FAIL: decode_display_list returned true on a truncated frame\n";
            return false;
        }
        std::cerr << "ok: truncated frame rejected (decode returned false)\n";
        return true;
    }

} // namespace

// std::vector/std::string here can in principle throw std::bad_alloc; a test main treats that as a
// crash-fail, exactly like the sibling host-linked tests.
// NOLINTNEXTLINE(bugprone-exception-escape)
int main() {
    const bool all_passed = decodes_a_rich_frame() && rejects_a_truncated_frame();
    if (all_passed) {
        std::cerr << "ok: C++ display-list decoder round-trips the real engine's frame\n";
        return 0;
    }
    return 1;
}
