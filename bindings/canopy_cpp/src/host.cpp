#include "canopy_cpp/host.hpp"

#include <cstddef>
#include <cstdint>
#include <vector>

#include "canopy.h"

#include "canopy_cpp/build_context.hpp"
#include "canopy_cpp/event.hpp"

namespace canopy {

    void host::apply(const std::vector<std::uint8_t>& batch) {
        canopy_host_apply(handle_.get(), batch.data(), batch.size());
    }

    void host::resize(float width, float height) {
        canopy_host_resize(handle_.get(), width, height);
    }

    auto host::pointer(float pos_x, float pos_y, std::uint8_t button, std::uint16_t event) -> int {
        return canopy_host_pointer(handle_.get(), pos_x, pos_y, button, event);
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

} // namespace canopy
