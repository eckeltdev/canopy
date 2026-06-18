#include "canopy_cpp/host.hpp"

#include <cstddef>
#include <cstdint>
#include <string_view>
#include <vector>

#include "canopy.h"

#include "canopy_cpp/build_context.hpp"
#include "canopy_cpp/event.hpp"
#include "canopy_cpp/reactive.hpp"

namespace canopy {

    void host::apply(const std::vector<std::uint8_t>& batch) {
        canopy_host_apply(handle_.get(), batch.data(), batch.size());
    }

    void host::resize(float width, float height) {
        canopy_host_resize(handle_.get(), width, height);
    }

    void host::set_stylesheet(std::string_view css) {
        // The C ABI takes UTF-8 bytes (const uint8_t*); copy the char view into a byte buffer
        // rather than reinterpret-cast char* -> uint8_t* (a tiny, infrequently-set stylesheet).
        const std::vector<std::uint8_t> bytes(css.begin(), css.end());
        canopy_host_set_stylesheet(handle_.get(), bytes.data(), bytes.size());
    }

    auto host::pointer(float pos_x, float pos_y, std::uint8_t button, std::uint16_t event) -> int {
        return canopy_host_pointer(handle_.get(), pos_x, pos_y, button, event);
    }

    auto host::hover(float pos_x, float pos_y) -> bool {
        return canopy_host_hover(handle_.get(), pos_x, pos_y) > 0;
    }

    auto host::pump(build_context& ctx) -> int {
        std::vector<std::uint8_t> buf(256);
        std::size_t len = 0;
        int code = canopy_host_poll_events(handle_.get(), buf.data(), buf.size(), &len);
        if (code == CANOPY_ERR_TOO_LARGE) { // grow once to the reported size and retry
            buf.resize(len);
            code = canopy_host_poll_events(handle_.get(), buf.data(), buf.size(), &len);
        }
        if (code != CANOPY_OK || len == 0) {
            return 0;
        }
        // Install the context's runtime as active while handlers run, so a handler's `signal.set`
        // discovers the runtime through the seam and marks its bound effects dirty. The caller then
        // `flush`es to emit the surgical update ops. Without this scope a `set` outside a build
        // pass would no-op and the reactive update would be lost.
        const active_runtime_scope scope(&ctx.runtime());
        int fired = 0;
        decode_event_batch(buf.data(), len, [&](const dispatch_event& event) {
            if (ctx.invoke_handler(event.handler)) {
                fired += 1;
            }
        });
        return fired;
    }

    auto host::node_count() const -> std::size_t {
        return canopy_host_node_count(handle_.get());
    }

    auto host::render_rgba(std::uint32_t width, std::uint32_t height) const
        -> std::vector<std::uint8_t> {
        // The frame is exactly width*height*4 bytes, so size the buffer up front and the single
        // call fills it (the needed-size contract only forces a second call when cap is short).
        const std::size_t wide = width;
        const std::size_t tall = height;
        std::vector<std::uint8_t> pixels(wide * tall * 4U);
        std::size_t len = 0;
        const int code = canopy_host_render_rgba(handle_.get(), width, height, pixels.data(),
                                                 pixels.size(), &len);
        if (code != CANOPY_OK) {
            pixels.clear(); // zero/oversize dimension or a null path — hand back an empty frame
        }
        return pixels;
    }

} // namespace canopy
