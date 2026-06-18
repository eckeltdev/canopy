#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string_view>
#include <vector>

#include "canopy.h" // CanopyHost (opaque) + canopy_host_* C ABI

#include "canopy_cpp/build_context.hpp"

// A move-only RAII owner of a real Canopy engine host (canopy-abi). It applies op batches,
// drives pointer input + the viewport, and `pump`s queued host -> guest events into the C++
// handler closures parked in a build_context's handler table. Link libcanopy_abi.a.
namespace canopy {

    class host {
    public:
        host() : handle_(canopy_host_new()) {}

        // Apply one op batch (e.g. from build_context::take_batch) to the engine.
        void apply(const std::vector<std::uint8_t>& batch);

        // Set the viewport used for hit-testing (logical pixels).
        void resize(float width, float height);

        // Install a CSS-lite class stylesheet (`.class { prop: value }` rules). Subsequent
        // render/pointer cascade each node's classes through it (author inline styles win); the
        // retained tree is unchanged. Pass an empty view to clear the stylesheet.
        void set_stylesheet(std::string_view css);

        // Deliver a pointer event; the engine hit-tests and queues a DispatchEvent if it lands
        // on a node with a matching listener. Returns the number of events queued (0 or 1).
        auto pointer(float pos_x, float pos_y, std::uint8_t button, std::uint16_t event) -> int;

        // Drain queued events and invoke the stored handler for each, looked up in `ctx`'s
        // handler table (the guest-minted handler ids round-trip). Returns the number fired.
        auto pump(build_context& ctx) -> int;

        // Live node count in the engine's retained tree (excluding the implicit root).
        [[nodiscard]] auto node_count() const -> std::size_t;

        // Render the engine's current tree to an RGBA8 framebuffer: `width * height * 4` bytes,
        // row-major, straight alpha — what a desktop viewer or a device framebuffer blits. Uses
        // the lite layout + software rasterizer (the same geometry the hit-test reads). Returns an
        // empty vector if a dimension is zero or exceeds the engine's render cap.
        [[nodiscard]] auto render_rgba(std::uint32_t width, std::uint32_t height) const
            -> std::vector<std::uint8_t>;

    private:
        // A custom deleter that frees via the C ABI (never `delete`), so the opaque CanopyHost
        // never needs to be a complete type here.
        struct deleter {
            void operator()(CanopyHost* ptr) const noexcept {
                canopy_host_free(ptr);
            }
        };
        std::unique_ptr<CanopyHost, deleter> handle_;
    };

} // namespace canopy
