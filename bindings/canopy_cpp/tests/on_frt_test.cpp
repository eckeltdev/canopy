#include <cstdint>
#include <iostream>
#include <vector>

#include "frt/debug_backend.hpp"

#include "canopy_cpp/build_context.hpp"
#include "canopy_cpp/dsl.hpp"

// P5 proof: the Canopy C++ binding runs ON frt. This target links NO engine — it compiles
// build_context.cpp together with frt's global operator new/delete (gated by FRT_OWN_NEW_DELETE)
// and the frt platform seam, so every std::vector / std::map / std::string the binding touches
// allocates through frt_platform_alloc/free. We install frt's debug-tracking backend BEFORE
// building a tree, then prove two things at once:
//   (1) the encoder still produces the exact wire bytes (build_context unchanged), and
//   (2) those allocations flowed through the frt seam (the debug backend counted them) and
//       balanced once the context was destroyed (no leak, no sized/aligned-free mismatch).
namespace {

    using bytes = std::vector<std::uint8_t>;

    void dump(const char* label, const bytes& data) {
        std::cerr << "  " << label << " (" << data.size() << "):" << std::hex;
        for (const std::uint8_t value : data) {
            std::cerr << ' ' << static_cast<int>(value);
        }
        std::cerr << std::dec << '\n';
    }

    auto check_bytes(const bytes& got, const bytes& want, const char* what) -> bool {
        if (got == want) {
            return true;
        }
        std::cerr << "FAIL: " << what << '\n';
        dump("got ", got);
        dump("want", want);
        return false;
    }

    // The known-good wire pattern for a single COLUMN appended under ROOT — the identical byte
    // contract the encoder oracle pins (tests/encoder_test.cpp::minimal_tree_is_byte_exact). If
    // routing allocation through frt changed even one emitted byte, this catches it.
    auto minimal_tree_want() -> bytes {
        return {
            // BeginBatch(version = 1, seq = 0)
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            // CreateElement(node = 1, tag = COLUMN = 1)
            0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            // InsertBefore(parent = ROOT = 0, child = 1, anchor = NULL)
            0x13, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            // EndBatch
            0x02,
        };
    }

    // Build the SAME minimal tree two ways — once via the builder DSL, once via direct
    // build_context calls — and assert both equal the pinned bytes. Every std container in
    // play (the op buffer, the intern map, the handler vector, the reactive runtime box) is
    // allocating through frt while this runs.
    auto encoder_is_byte_exact_on_frt() -> bool {
        const bytes want = minimal_tree_want();

        bytes via_dsl;
        {
            canopy::build_context ctx;
            canopy::mount(ctx, canopy::div());
            via_dsl = ctx.take_batch(0);
        }

        bytes via_direct;
        {
            canopy::build_context ctx;
            const canopy::node_id col = ctx.create_element(canopy::wire::el_column);
            ctx.append(canopy::root, col);
            via_direct = ctx.take_batch(0);
        }

        const bool dsl_ok = check_bytes(via_dsl, want, "DSL minimal tree on frt");
        const bool direct_ok = check_bytes(via_direct, want, "direct minimal tree on frt");
        const bool parity_ok = check_bytes(via_dsl, via_direct, "DSL == direct on frt");
        return dsl_ok && direct_ok && parity_ok;
    }

    // The headline proof: install the tracking backend, snapshot the counters, build a richer
    // tree (a card with a class, a button carrying a click handler, and a text leaf — string
    // interning, a handler vector slot, and the reactive runtime all exercised) inside a scope,
    // take its batch, then DESTROY the context. Assert (a) allocations rose (the binding's std
    // containers really did allocate through frt's seam), (b) the live set returned to where it
    // started (the context's allocations were all freed), and (c) no sized/aligned free disagreed
    // with its alloc (the advisory-hint contract held end to end).
    auto bindings_allocations_flow_through_frt() -> bool {
        frt::install_debug_backend();

        // Warm-up: build and tear down one throwaway tree BEFORE the measured window. Any
        // one-time lazily-initialized runtime allocation (a function-local static inside libc++
        // or the binding) happens here, so it is already live when we snapshot `start` and never
        // counts as a leak in the measured delta. The reset AFTER the warm-up zeroes the counters
        // so the numbers we assert on describe exactly the measured scope.
        {
            canopy::build_context warm;
            canopy::mount(warm, canopy::div(canopy::cls("card"),
                                            canopy::button(canopy::on_click([] {}), "Click")));
            static_cast<void>(warm.take_batch(0));
        }
        frt::debug_backend_reset();

        const frt::debug_stats start = frt::debug_backend_stats();
        bool produced_bytes = false;
        {
            canopy::build_context ctx;
            canopy::mount(ctx, canopy::div(canopy::cls("card"),
                                           canopy::button(canopy::on_click([] {}), "Click")));
            // The returned batch vector is itself seam-allocated; it is scoped INSIDE the measured
            // window so it is destroyed (its block freed) before the `finish` snapshot. We only
            // carry a bool out, so nothing seam-allocated survives to skew the live-set balance.
            const bytes batch = ctx.take_batch(0);
            produced_bytes = !batch.empty();
        } // ctx, its batch, and everything it owns destroyed here -> their frt allocations freed
        const frt::debug_stats finish = frt::debug_backend_stats();

        if (!produced_bytes) {
            std::cerr << "FAIL: built tree produced no bytes\n";
            return false;
        }
        if (finish.alloc_count <= start.alloc_count) {
            std::cerr << "FAIL: binding did not allocate through the frt seam (alloc_count "
                      << start.alloc_count << " -> " << finish.alloc_count << ")\n";
            return false;
        }
        if (finish.live_count != start.live_count) {
            std::cerr << "FAIL: leaked allocations after context destruction (live "
                      << start.live_count << " -> " << finish.live_count << ")\n";
            return false;
        }
        if (finish.mismatch_count != 0) {
            std::cerr << "FAIL: " << finish.mismatch_count
                      << " sized/aligned free mismatch(es) through the seam\n";
            return false;
        }

        std::cerr << "ok: binding allocated " << (finish.alloc_count - start.alloc_count)
                  << " block(s) through frt; live set balanced, hints matched\n";
        return true;
    }

} // namespace

int main() {
    const bool bytes_ok = encoder_is_byte_exact_on_frt();
    const bool seam_ok = bindings_allocations_flow_through_frt();
    if (bytes_ok && seam_ok) {
        std::cerr << "ok: canopy_cpp runs ON frt (wire bytes unchanged; allocations via the seam)\n";
        return 0;
    }
    return 1;
}
