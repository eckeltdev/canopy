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

project(canopy_rt LANGUAGES CXX VERSION 0.1.0)

# Target: canopy_rt
add_library(canopy_rt STATIC)
target_sources(canopy_rt PRIVATE "src/backend_host_posix.cpp" "src/platform.cpp")
target_include_directories(canopy_rt PUBLIC include)
target_compile_features(canopy_rt PUBLIC cxx_std_23)

# Target: canopy_rt_new_delete_test
add_executable(canopy_rt_new_delete_test)
target_sources(
	canopy_rt_new_delete_test
	PRIVATE
	"src/backend_debug_tracking.cpp"
	"src/new_delete.cpp"
	"tests/new_delete_route_test.cpp"
)
target_link_libraries(canopy_rt_new_delete_test PRIVATE canopy_rt)
target_include_directories(canopy_rt_new_delete_test PRIVATE include)
target_compile_features(canopy_rt_new_delete_test PRIVATE cxx_std_23)
target_compile_options(canopy_rt_new_delete_test PRIVATE -DCANOPY_RT_OWN_NEW_DELETE)

# Target: canopy_rt_platform_test
add_executable(canopy_rt_platform_test)
target_sources(canopy_rt_platform_test PRIVATE "tests/platform_host_test.cpp")
target_link_libraries(canopy_rt_platform_test PRIVATE canopy_rt)
target_include_directories(canopy_rt_platform_test PRIVATE include)
target_compile_features(canopy_rt_platform_test PRIVATE cxx_std_23)

# Target: canopy_rt_surface_test
add_executable(canopy_rt_surface_test)
target_sources(canopy_rt_surface_test PRIVATE "tests/rt_surface_smoke.cpp")
target_link_libraries(canopy_rt_surface_test PRIVATE canopy_rt)
target_include_directories(canopy_rt_surface_test PRIVATE include)
target_compile_features(canopy_rt_surface_test PRIVATE cxx_std_23)
target_compile_options(
	canopy_rt_surface_test
	PRIVATE
	-fno-exceptions
	-fno-rtti
	-DCANOPY_RT_FREESTANDING=1
)
