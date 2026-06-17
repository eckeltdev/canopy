#include <cstddef>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#include "canopy.h" // the Rust C ABI (canopy-abi)

#include "canopy_cpp/dsl.hpp"

// A worked example of the full Canopy C++ authoring story: a component-based UI is built with
// the DSL, its op bytes are applied through the REAL engine (canopy-abi), and the resulting Dom
// is dumped. Build the engine staticlib first: `cargo build -p canopy-abi`.
namespace {

    // A small reusable component: a card with a title and a [-] value [+] control row, authored
    // entirely with the builder DSL. `build` splices the subtree under `parent` (the Component
    // contract).
    struct counter_card {
        int count = 0;
        void build(canopy::build_context& ctx, canopy::node_id parent) const {
            canopy::mount(
                ctx, parent,
                canopy::div(canopy::cls("card"), canopy::text("Counter"),
                            canopy::row(canopy::cls("controls"),
                                        canopy::button(canopy::on_click([] {}), "-"),
                                        canopy::text(std::to_string(count)),
                                        canopy::button(canopy::on_click([] {}), "+"))));
        }
    };

    // Apply `batch` through a fresh real host and return its deterministic tree dump.
    std::string render_to_snapshot(const std::vector<std::uint8_t>& batch) {
        CanopyHost* host = canopy_host_new();
        canopy_host_apply(host, batch.data(), batch.size());
        std::vector<std::uint8_t> buf(512);
        std::size_t len = 0;
        if (canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len) == CANOPY_ERR_TOO_LARGE) {
            buf.resize(len);
            canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        }
        canopy_host_free(host);
        using diff = std::vector<std::uint8_t>::difference_type;
        return {buf.begin(), buf.begin() + static_cast<diff>(len)};
    }

} // namespace

int main() {
    canopy::build_context ctx;
    counter_card{.count = 3}.build(ctx, canopy::root); // splice directly under the host root

    const std::string tree = render_to_snapshot(ctx.take_batch(0));
    std::cout << "canopy C++ DSL -> real engine -> Dom:\n\n" << tree << '\n';

    // The example doubles as a regression guard: the engine's view of the authored tree is
    // fixed (handler ids 0/1 for the two buttons; row/buttons are tags 2/3).
    const std::string expected = "el tag=1 class=card\n"
                                 "  text=Counter\n"
                                 "  el tag=2 class=controls\n"
                                 "    el tag=3 on=1:0\n"
                                 "      text=-\n"
                                 "    text=3\n"
                                 "    el tag=3 on=1:1\n"
                                 "      text=+\n";
    if (tree != expected) {
        std::cerr << "example mismatch — expected:\n" << expected;
        return 1;
    }
    return 0;
}
