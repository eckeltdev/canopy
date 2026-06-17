#include <cstdint>
#include <string>
#include <vector>

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/signal.hpp"

// Freestanding compile gate. This translation unit (and the encoder it pulls in) is built with
// -fno-exceptions -fno-rtti. If any DSL / handler / encoder path used a throw, try/catch,
// typeid, or dynamic_cast, this would FAIL TO COMPILE — turning a freestanding portability
// break into a host build error now instead of an Orange Pi 5 surprise at P5. It exercises
// every DSL path, with emphasis on the on_click closure (the heap-free inplace_function store).
namespace {

    struct counter {
        int initial_value = 0;
        void build(canopy::build_context& ctx, canopy::node_id parent) const {
            ctx.append(parent, ctx.create_text(std::to_string(initial_value)));
        }
    };

} // namespace

int main() {
    canopy::signal<int> count{1};
    canopy::build_context ctx;
    canopy::mount(ctx, canopy::div(canopy::cls("card"), counter{.initial_value = 3},
                                   canopy::button(canopy::on_click([&] { count.set(count.get() + 1); }),
                                                  "Click"),
                                   canopy::text([&] { return std::to_string(count.get()); })));
    const std::vector<std::uint8_t> batch = ctx.take_batch(0);
    return batch.empty() ? 1 : 0;
}
