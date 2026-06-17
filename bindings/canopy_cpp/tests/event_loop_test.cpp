#include <cstdint>
#include <iostream>
#include <vector>

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/event.hpp"
#include "canopy_cpp/host.hpp"

namespace {

    // A hand-rolled poll_events batch proves the decoder reads handler(u32) BEFORE node(u64) —
    // the field-order anomaly — independent of the engine.
    bool decoder_honors_handler_before_node() {
        const std::vector<std::uint8_t> batch = {
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,       // BeginBatch v1 seq0
            0x80,                                           // DispatchEvent
            0x05, 0x00, 0x00, 0x00,                         // handler = 5 (u32, FIRST)
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // node = 1 (u64, SECOND)
            0x01,                                           // payload kind = pointer
            0x00, 0x00, 0x20, 0x41,                         // x = 10.0f
            0x00, 0x00, 0x70, 0x41,                         // y = 15.0f
            0x00,                                           // button = 0
            0x02,                                           // EndBatch
        };
        canopy::dispatch_event got;
        int count = 0;
        const auto status = canopy::decode_event_batch(
            batch.data(), batch.size(), [&](const canopy::dispatch_event& event) {
                got = event;
                count += 1;
            });
        if (status != canopy::decode_status::ok || count != 1) {
            std::cerr << "FAIL: decode status/count (count=" << count << ")\n";
            return false;
        }
        if (got.handler.raw != 5 || got.node.raw != 1 ||
            got.payload_kind != canopy::wire::payload_pointer) {
            std::cerr << "FAIL: decoded fields (handler=" << got.handler.raw
                      << " node=" << got.node.raw << ")\n";
            return false;
        }
        if (got.pointer.pos_x != 10.0F || got.pointer.pos_y != 15.0F || got.pointer.button != 0) {
            std::cerr << "FAIL: pointer payload\n";
            return false;
        }
        return true;
    }

    // End to end through the real engine: a pointer inside an INLINE-STYLED button (class-styled
    // trees have no hit geometry yet) fires its stored on_click closure; a miss fires nothing.
    bool a_pointer_inside_a_button_fires_its_handler() {
        int clicks = 0;
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::button(canopy::style(canopy::wire::prop_width, "100"),
                                          canopy::style(canopy::wire::prop_height, "40"),
                                          canopy::on_click([&] { clicks += 1; }), "x"));
        canopy::host engine;
        engine.apply(ctx.take_batch(0));
        engine.resize(200.0F, 200.0F);

        engine.pointer(10.0F, 10.0F, 0, canopy::wire::event_click); // inside the 100x40 button
        const int fired = engine.pump(ctx);
        if (fired != 1 || clicks != 1) {
            std::cerr << "FAIL: inside hit (fired=" << fired << " clicks=" << clicks << ")\n";
            return false;
        }

        engine.pointer(150.0F, 150.0F, 0, canopy::wire::event_click); // outside the button
        const int missed = engine.pump(ctx);
        if (missed != 0 || clicks != 1) {
            std::cerr << "FAIL: outside miss (missed=" << missed << " clicks=" << clicks << ")\n";
            return false;
        }
        return true;
    }

} // namespace

int main() {
    const bool all_passed =
        decoder_honors_handler_before_node() && a_pointer_inside_a_button_fires_its_handler();
    if (all_passed) {
        std::cerr << "ok: event loop tests passed\n";
        return 0;
    }
    return 1;
}
