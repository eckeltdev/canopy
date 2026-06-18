#include <iostream>
#include <map>
#include <string>
#include <vector>

#include "frt/debug_backend.hpp"

// The headline M1 proof: with the frt global operator new/delete linked in, plain std
// containers allocate through the platform seam. We install a tracking backend, churn a vector
// / map / string in a scope, then assert (a) they DID allocate through the seam, (b) every one
// of those allocations was freed (live set returned to its prior size), and (c) no sized/aligned
// delete disagreed with its alloc (the advisory-(size,align) contract held).
namespace {

    bool std_containers_route_through_the_seam() {
        frt::install_debug_backend();
        frt::debug_backend_reset();

        const frt::debug_stats start = frt::debug_backend_stats();
        {
            std::vector<int> numbers;
            // Intentionally NOT reserved: the realloc churn exercises the allocator's
            // alloc-new / free-old path through the seam, which is part of what we verify.
            for (int idx = 0; idx < 1000; ++idx) {
                // NOLINTNEXTLINE(performance-inefficient-vector-operation)
                numbers.push_back(idx);
            }
            std::map<std::string, int, std::less<>> table;
            table.emplace("alpha", 1);
            table.emplace("beta", 2);
            std::string text(4096, 'x');
            static_cast<void>(numbers.size());
            static_cast<void>(table.size());
            static_cast<void>(text.size());
        } // every container destroyed here -> its seam allocations freed

        const frt::debug_stats finish = frt::debug_backend_stats();

        if (finish.alloc_count <= start.alloc_count) {
            std::cerr << "FAIL: std containers did not allocate through the seam\n";
            return false;
        }
        if (finish.live_count != start.live_count) {
            std::cerr << "FAIL: leaked allocations (live " << start.live_count << " -> "
                      << finish.live_count << ")\n";
            return false;
        }
        if (finish.mismatch_count != 0) {
            std::cerr << "FAIL: " << finish.mismatch_count << " sized/aligned free mismatch(es)\n";
            return false;
        }
        return true;
    }

} // namespace

int main() {
    if (std_containers_route_through_the_seam()) {
        std::cerr << "ok: std containers route through frt (alloc==free, hints matched)\n";
        return 0;
    }
    return 1;
}
