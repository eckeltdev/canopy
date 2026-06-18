#include "frt/platform.hpp"

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>

// The host-POSIX backend: the default frt platform on a desktop/CI host. It is the
// floor the whole runtime is developed and tested against before any device exists.
namespace frt {

    namespace {

        auto host_alloc(std::size_t size, std::size_t align) -> void* {
            void* ptr = nullptr;
            // posix_memalign needs alignment >= sizeof(void*) and a power of two; new's default
            // alignment already satisfies that, but clamp small alignments defensively.
            const std::size_t aligned = align < sizeof(void*) ? sizeof(void*) : align;
            // This backend IS the allocator, so it calls the C allocator directly.
            // NOLINTNEXTLINE(cppcoreguidelines-no-malloc,cppcoreguidelines-owning-memory,misc-include-cleaner)
            if (::posix_memalign(&ptr, aligned, size) != 0) {
                return nullptr;
            }
            return ptr;
        }

        // size/align are advisory per the seam contract; the host backend ignores them.
        void host_free(void* ptr, std::size_t /*size*/, std::size_t /*align*/) {
            // This backend IS the allocator, so it calls the C allocator directly.
            // NOLINTNEXTLINE(cppcoreguidelines-no-malloc,cppcoreguidelines-owning-memory)
            std::free(ptr);
        }

        void host_panic(const char* msg) {
            static_cast<void>(std::fputs("frt panic: ", stderr));
            static_cast<void>(std::fputs(msg != nullptr ? msg : "(null)", stderr));
            static_cast<void>(std::fputc('\n', stderr));
            std::abort();
        }

        void host_log(const char* msg, std::size_t len) {
            static_cast<void>(std::fwrite(msg, 1, len, stderr));
        }

        auto host_ticks() -> std::uint64_t {
            return static_cast<std::uint64_t>(
                std::chrono::steady_clock::now().time_since_epoch().count());
        }

        auto host_ticks_per_second() -> std::uint64_t {
            using period = std::chrono::steady_clock::period;
            return static_cast<std::uint64_t>(period::den) /
                   static_cast<std::uint64_t>(period::num);
        }

        constexpr platform_ops host_ops_table = {
            .alloc = host_alloc,
            .free = host_free,
            .panic = host_panic,
            .log = host_log,
            .ticks = host_ticks,
            .ticks_per_second = host_ticks_per_second,
        };

    } // namespace

    auto host_ops() -> const platform_ops& {
        return host_ops_table;
    }

} // namespace frt
