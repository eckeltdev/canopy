#pragma once

#include <cstddef>
#include <cstdint>

#include "frt/frt_platform.h"

// The installable-backend layer over the C platform seam. The C symbols (frt_platform_*)
// forward to whatever `platform_ops` is currently active, which defaults to the host-POSIX
// backend and can be swapped at runtime (e.g. a test installs an instrumented backend without
// relinking). On the bare-metal device the HAL installs its own ops at startup.
namespace frt {

    // A backend: the platform primitives a target supplies. All-public function-pointer
    // aggregate (a POD vtable). `panic` must not return (the C seam enforces that).
    struct platform_ops {
        void* (*alloc)(std::size_t size, std::size_t align);
        void (*free)(void* ptr, std::size_t size, std::size_t align);
        void (*panic)(const char* msg);
        void (*log)(const char* msg, std::size_t len);
        std::uint64_t (*ticks)();
        std::uint64_t (*ticks_per_second)();
    };

    // The host-POSIX default backend (posix_memalign / stderr / steady_clock / abort). Used
    // until something is installed. Defined in backend_host_posix.cpp.
    [[nodiscard]] auto host_ops() -> const platform_ops&;

    // Install `ops` as the active backend; returns the previously-installed pointer (nullptr
    // if none was explicitly installed). Thread-safe.
    auto install_platform(const platform_ops& ops) -> const platform_ops*;

    // The active backend — the installed one, or host_ops() if none installed. Never null.
    [[nodiscard]] auto platform() -> const platform_ops&;

    // Typed conveniences over the active backend.
    inline void log(const char* msg, std::size_t len) {
        platform().log(msg, len);
    }
    [[nodiscard]] inline auto ticks_now() -> std::uint64_t {
        return platform().ticks();
    }

} // namespace frt
