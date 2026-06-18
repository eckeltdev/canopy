#include "frt/xstd.hpp" // configure xstd for frt — MUST precede any <xstd/*.hpp> include

#include <cstdint>
#include <iostream>

#include "xstd/bitwise.hpp"
#include "xstd/fnv.hpp"

// Proof that the vendored xstd extended STL compiles AND runs on frt under the freestanding
// configuration (-fno-exceptions -fno-rtti): exercise a couple of pure, allocation-free xstd
// utilities and assert known results. This is the curated-safe subset; the assert/logger/
// formatting chain is gated (see frt/xstd.hpp).
namespace {

    bool xstd_bitwise_and_fnv_work() {
        // popcnt: 0b1011 has three set bits.
        if (xstd::popcnt(std::uint32_t{0b1011}) != 3) {
            std::cerr << "FAIL: xstd::popcnt\n";
            return false;
        }

        // fnv64 (xstd::fnv1a<uint64_t,...>) is a constexpr hasher: the same input hashes
        // deterministically, a different input differs. Evaluated at compile time to prove it is
        // usable in constant contexts.
        constexpr std::uint64_t hash_a = xstd::fnv64{}.update(std::uint32_t{42}).digest();
        constexpr std::uint64_t hash_b = xstd::fnv64{}.update(std::uint32_t{42}).digest();
        constexpr std::uint64_t hash_c = xstd::fnv64{}.update(std::uint32_t{43}).digest();
        static_assert(hash_a == hash_b, "fnv64 is deterministic");
        static_assert(hash_a != hash_c, "fnv64 distinguishes inputs");
        if (hash_a == 0) {
            std::cerr << "FAIL: xstd::fnv1a produced a zero digest\n";
            return false;
        }
        return true;
    }

} // namespace

int main() {
    if (xstd_bitwise_and_fnv_work()) {
        std::cerr << "ok: xstd (bitwise + fnv) compiles and runs on frt, freestanding\n";
        return 0;
    }
    return 1;
}
