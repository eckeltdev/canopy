#include <cstddef>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#include "canopy.h" // the Rust C ABI (canopy-abi)

#include "canopy_cpp/dsl.hpp"

// The P2 round-trip oracle: build a tree with the C++ DSL, apply its op bytes through the
// REAL Rust engine (canopy_host_apply), and assert the engine's deterministic tree dump
// matches what we intended. This proves the C++-emitted bytes are not just self-consistent
// (the byte oracles in dsl_test) but are accepted and built correctly by the actual host —
// and that they yield the SAME tree the Rust Emitter does for the equivalent authoring.
namespace {

    // Apply `batch` through a fresh host and return its debug snapshot (or a marker on error).
    std::string snapshot_of(const std::vector<std::uint8_t>& batch) {
        CanopyHost* host = canopy_host_new();
        const int apply_rc = canopy_host_apply(host, batch.data(), batch.size());
        if (apply_rc != CANOPY_OK) {
            canopy_host_free(host);
            std::cerr << "FAIL: canopy_host_apply returned " << apply_rc << '\n';
            return "<apply-error>";
        }

        std::vector<std::uint8_t> buf(256);
        std::size_t len = 0;
        int snap_rc = canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        if (snap_rc == CANOPY_ERR_TOO_LARGE) { // grow once to the reported size and retry
            buf.resize(len);
            snap_rc = canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        }
        canopy_host_free(host);
        if (snap_rc != CANOPY_OK) {
            std::cerr << "FAIL: canopy_host_debug_snapshot returned " << snap_rc << '\n';
            return "<snapshot-error>";
        }
        using diff = std::vector<std::uint8_t>::difference_type;
        return {buf.begin(), buf.begin() + static_cast<diff>(len)};
    }

    bool dsl_tree_round_trips_through_the_real_engine() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::cls("card"),
                                       canopy::button(canopy::on_click([] {}), "Click")));

        const std::string got = snapshot_of(ctx.take_batch(0));
        const std::string want = "el tag=1 class=card\n  el tag=3 on=1:0\n    text=Click\n";
        if (got != want) {
            std::cerr << "FAIL: round-trip mismatch\n--- got ---\n"
                      << got << "--- want ---\n"
                      << want;
            return false;
        }
        return true;
    }

    // The id/style modifiers reach the real engine, and the host returns styles in ascending
    // PropId order (a BTreeMap) regardless of the source order they were authored in — wire-byte
    // order (source) is NOT the host-visible snapshot order.
    bool modifiers_round_trip_in_host_sorted_order() {
        canopy::build_context ctx;
        // width (PropId 3) is authored BEFORE bg (PropId 1); the host re-sorts by id.
        canopy::mount(ctx, canopy::div(canopy::id("main"),
                                       canopy::style(canopy::wire::prop_width, "10"),
                                       canopy::style(canopy::wire::prop_bg, "#fff")));

        const std::string got = snapshot_of(ctx.take_batch(0));
        const std::string want = "el tag=1 style=1:#fff;3:10 attr=1:main\n";
        if (got != want) {
            std::cerr << "FAIL: modifiers round-trip\n--- got ---\n"
                      << got << "--- want ---\n"
                      << want;
            return false;
        }
        return true;
    }

} // namespace

int main() {
    const bool all_passed =
        dsl_tree_round_trips_through_the_real_engine() && modifiers_round_trip_in_host_sorted_order();
    if (all_passed) {
        std::cerr << "ok: DSL trees round-trip through the real engine\n";
        return 0;
    }
    return 1;
}
