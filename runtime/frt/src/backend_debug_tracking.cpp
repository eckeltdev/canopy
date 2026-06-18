#include "frt/debug_backend.hpp"

#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <functional>
#include <map>
#include <utility>

#include "frt/platform.hpp"

namespace frt {

    namespace {

        // Allocator that uses raw malloc/free directly, so the tracking map's OWN storage never
        // re-enters the intercepted operator-new path (which routes back into this backend and
        // would recurse). This is the one place that deliberately bypasses the runtime.
        template <class T> struct raw_allocator {
            using value_type = T;
            raw_allocator() = default;
            template <class U> explicit raw_allocator(const raw_allocator<U>& /*other*/) noexcept {}
            auto allocate(std::size_t count) -> T* {
                // NOLINTNEXTLINE(cppcoreguidelines-no-malloc,cppcoreguidelines-owning-memory)
                return static_cast<T*>(std::malloc(count * sizeof(T)));
            }
            void deallocate(T* ptr, std::size_t /*count*/) noexcept {
                // NOLINTNEXTLINE(cppcoreguidelines-no-malloc,cppcoreguidelines-owning-memory)
                std::free(ptr);
            }
            template <class U>
            auto operator==(const raw_allocator<U>& /*other*/) const noexcept -> bool {
                return true;
            }
        };

        struct alloc_info {
            std::size_t size;
            std::size_t align;
        };

        using live_map = std::map<void*, alloc_info, std::less<>,
                                  raw_allocator<std::pair<void* const, alloc_info>>>;

        struct debug_state {
            live_map live;
            std::size_t alloc_count = 0;
            std::size_t free_count = 0;
            std::size_t mismatch_count = 0;
        };

        // Function-local static — avoids a mutable namespace-scope global.
        auto state() -> debug_state& {
            static debug_state instance;
            return instance;
        }

        auto debug_alloc(std::size_t size, std::size_t align) -> void* {
            void* ptr = host_ops().alloc(size, align);
            if (ptr != nullptr) {
                debug_state& self = state();
                self.live.insert_or_assign(ptr, alloc_info{.size = size, .align = align});
                self.alloc_count += 1;
            }
            return ptr;
        }

        void debug_free(void* ptr, std::size_t size, std::size_t align) {
            if (ptr != nullptr) {
                debug_state& self = state();
                if (auto found = self.live.find(ptr); found != self.live.end()) {
                    // size/align are advisory: only a NON-ZERO hint that disagrees is a defect.
                    if ((size != 0 && size != found->second.size) ||
                        (align != 0 && align != found->second.align)) {
                        self.mismatch_count += 1;
                    }
                    self.live.erase(found);
                }
                self.free_count += 1;
            }
            host_ops().free(ptr, size, align);
        }

        void debug_panic(const char* msg) {
            host_ops().panic(msg);
        }
        void debug_log(const char* msg, std::size_t len) {
            host_ops().log(msg, len);
        }
        auto debug_ticks() -> std::uint64_t {
            return host_ops().ticks();
        }
        auto debug_ticks_per_second() -> std::uint64_t {
            return host_ops().ticks_per_second();
        }

        constexpr platform_ops debug_ops_table = {
            .alloc = debug_alloc,
            .free = debug_free,
            .panic = debug_panic,
            .log = debug_log,
            .ticks = debug_ticks,
            .ticks_per_second = debug_ticks_per_second,
        };

    } // namespace

    auto install_debug_backend() -> const platform_ops* {
        return install_platform(debug_ops_table);
    }

    auto debug_backend_stats() -> debug_stats {
        const debug_state& self = state();
        return debug_stats{.alloc_count = self.alloc_count,
                           .free_count = self.free_count,
                           .live_count = self.live.size(),
                           .mismatch_count = self.mismatch_count};
    }

    void debug_backend_reset() {
        debug_state& self = state();
        self.live.clear();
        self.alloc_count = 0;
        self.free_count = 0;
        self.mismatch_count = 0;
    }

} // namespace frt
