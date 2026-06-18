# Generated from cpp-doctor.toml - DO NOT EDIT
# Edit cpp-doctor.toml and re-run `cpp-doctor gen` instead.

cmake_minimum_required(VERSION 3.15...3.31)

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_BINARY_DIR)
	message(
		FATAL_ERROR
		"In-tree builds are not supported. Run CMake from a separate directory: cmake -B build"
	)
endif()

set(CPPDOCTOR_ROOT_PROJECT OFF)
if(CMAKE_CURRENT_SOURCE_DIR STREQUAL CMAKE_SOURCE_DIR)
	set(CPPDOCTOR_ROOT_PROJECT ON)

	# Bootstrap cpp-doctor and automatically regenerate CMakeLists.txt
	include("cmake/cpp-doctor.cmake" OPTIONAL RESULT_VARIABLE CPPDOCTOR_INCLUDE_RESULT)
	if(CPPDOCTOR_INCLUDE_RESULT)
		cpp_doctor()
	endif()

	# Enable IDE folder support
	set_property(GLOBAL PROPERTY USE_FOLDERS ON)

	# Export a compile database for cpp-doctor, clang-tidy, and IDEs
	set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

	# Re-run when cpp-doctor.toml changes (IDE dependency)
	set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS cpp-doctor.toml)
endif()

project(frt LANGUAGES CXX VERSION 0.1.0)

# Target: frt
add_library(frt STATIC)
target_sources(frt PRIVATE "src/backend_host_posix.cpp" "src/platform.cpp")
target_include_directories(frt PUBLIC include)
target_compile_features(frt PUBLIC cxx_std_23)

# Target: frt_new_delete_test
add_executable(frt_new_delete_test)
target_sources(
	frt_new_delete_test
	PRIVATE
	"src/backend_debug_tracking.cpp"
	"src/new_delete.cpp"
	"tests/new_delete_route_test.cpp"
)
target_link_libraries(frt_new_delete_test PRIVATE frt)
target_include_directories(frt_new_delete_test PRIVATE include)
target_compile_features(frt_new_delete_test PRIVATE cxx_std_23)
target_compile_options(frt_new_delete_test PRIVATE -DFRT_OWN_NEW_DELETE)

# Target: frt_platform_test
add_executable(frt_platform_test)
target_sources(frt_platform_test PRIVATE "tests/platform_host_test.cpp")
target_link_libraries(frt_platform_test PRIVATE frt)
target_include_directories(frt_platform_test PRIVATE include)
target_compile_features(frt_platform_test PRIVATE cxx_std_23)

# Target: frt_surface_test
add_executable(frt_surface_test)
target_sources(frt_surface_test PRIVATE "tests/rt_surface_smoke.cpp")
target_link_libraries(frt_surface_test PRIVATE frt)
target_include_directories(frt_surface_test PRIVATE include)
target_compile_features(frt_surface_test PRIVATE cxx_std_23)
target_compile_options(frt_surface_test PRIVATE -fno-exceptions -fno-rtti -DFRT_FREESTANDING=1)

# Target: frt_xstd_test
add_executable(frt_xstd_test)
target_sources(frt_xstd_test PRIVATE "tests/xstd_surface_smoke.cpp")
target_include_directories(frt_xstd_test PRIVATE include "vendor/xstd")
target_compile_features(frt_xstd_test PRIVATE cxx_std_23)
target_compile_options(frt_xstd_test PRIVATE -fno-exceptions -fno-rtti)
