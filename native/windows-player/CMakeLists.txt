cmake_minimum_required(VERSION 3.24)
set(manager_manifest "${CMAKE_CURRENT_SOURCE_DIR}/../../apps/manager/Cargo.toml")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${manager_manifest}")
file(STRINGS "${manager_manifest}" version_line REGEX "^version = " LIMIT_COUNT 1)
string(REGEX MATCH "[0-9]+[.][0-9]+[.][0-9]+" app_version "${version_line}")
project(Action75Native VERSION "${app_version}" LANGUAGES CXX)
if(NOT WIN32)
    message(FATAL_ERROR "Action75Native currently targets Windows")
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
add_compile_definitions(UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN _WIN32_WINNT=0x0A00)
add_compile_options($<$<CXX_COMPILER_ID:MSVC>:/utf-8>)
get_filename_component(ACTION75_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(ACTION75_DIST_DIR "${ACTION75_ROOT}/dist" CACHE PATH "Native player output root")
add_library(action75_json INTERFACE)
target_include_directories(action75_json INTERFACE "${ACTION75_ROOT}/vendor-source/windows-native")
add_library(action75_input STATIC src/PlayerConfig.cpp src/RawKeyboard.cpp)
add_library(action75_settings STATIC src/PackSettings.cpp)
add_library(action75_display_guard STATIC src/DisplayGuard.cpp src/MonitorSelection.cpp)
target_include_directories(action75_display_guard PUBLIC include)
target_link_libraries(action75_display_guard PUBLIC user32 dwmapi setupapi advapi32)
target_include_directories(action75_settings PUBLIC include)
target_link_libraries(action75_settings PUBLIC action75_json)
set(release_assets "${ACTION75_ROOT}/tools/release-assets.json")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${release_assets}")
file(READ "${release_assets}" release_inventory)
string(JSON bundled_count LENGTH "${release_inventory}" wallpapers)
set(bundled_ids "")
if(bundled_count GREATER 0)
    math(EXPR last_bundled_index "${bundled_count} - 1")
    foreach(index RANGE ${last_bundled_index})
        string(JSON id MEMBER "${release_inventory}" wallpapers ${index})
        if(NOT id MATCHES "^[A-Za-z0-9_-]+$")
            message(FATAL_ERROR "Invalid wallpaper ID in release-assets.json: ${id}")
        endif()
        string(APPEND bundled_ids "    \"${id}\",\n")
    endforeach()
endif()
file(GENERATE OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/BundledWallpaperIds.h" CONTENT
    "#pragma once\n#include <array>\n#include <string_view>\ninline constexpr std::array<std::string_view, ${bundled_count}> BundledWallpaperIds = {\n${bundled_ids}};\n")
add_library(action75_wallpaper_catalog STATIC src/WallpaperCatalog.cpp)
target_include_directories(action75_wallpaper_catalog PUBLIC include PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(action75_wallpaper_catalog PUBLIC action75_settings)
target_include_directories(action75_input PUBLIC include)
target_link_libraries(action75_input PUBLIC action75_json user32)
include(cmake/Effekseer.cmake)
include(cmake/Backdrop.cmake)
set(water_shader "${CMAKE_CURRENT_SOURCE_DIR}/materials/WaterSurface.hlsl")
set(water_header "${CMAKE_CURRENT_BINARY_DIR}/WaterSurfaceShaders.h")
add_custom_command(OUTPUT "${water_header}"
    COMMAND action75_compile_fireworks_shaders "${water_shader}" "${water_header}" water
    DEPENDS action75_compile_fireworks_shaders "${water_shader}" "${CMAKE_CURRENT_SOURCE_DIR}/materials/LayerColor.hlsli" VERBATIM)
add_library(action75_water_surface STATIC src/WaterSurface.cpp "${water_header}")
target_include_directories(action75_water_surface PUBLIC include PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(action75_water_surface PUBLIC action75_json)
set(glass_shader "${CMAKE_CURRENT_SOURCE_DIR}/materials/GlassShatter.hlsl")
set(glass_header "${CMAKE_CURRENT_BINARY_DIR}/GlassShatterShaders.h")
add_custom_command(OUTPUT "${glass_header}"
    COMMAND action75_compile_fireworks_shaders "${glass_shader}" "${glass_header}" glass
    DEPENDS action75_compile_fireworks_shaders "${glass_shader}" VERBATIM)
add_library(action75_glass_shatter STATIC src/GlassShatter.cpp "${glass_header}")
target_include_directories(action75_glass_shatter PUBLIC include PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(action75_glass_shatter PUBLIC action75_json)
set(fireworks_shader "${CMAKE_CURRENT_SOURCE_DIR}/materials/Fireworks.hlsl")
set(fireworks_header "${CMAKE_CURRENT_BINARY_DIR}/FireworksShaders.h")
add_custom_command(OUTPUT "${fireworks_header}"
    COMMAND action75_compile_fireworks_shaders "${fireworks_shader}" "${fireworks_header}"
    DEPENDS action75_compile_fireworks_shaders "${fireworks_shader}" "${CMAKE_CURRENT_SOURCE_DIR}/materials/LayerColor.hlsli" VERBATIM)
add_library(action75_fireworks STATIC src/FireworksScene.cpp "${fireworks_header}")
target_include_directories(action75_fireworks PUBLIC include PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(action75_fireworks PUBLIC action75_json)
include(cmake/AtlasPacks.cmake)
include(cmake/SurfacePacks.cmake)
include(cmake/FieldPacks.cmake)
configure_file(cmake/app.rc.in "${CMAKE_CURRENT_BINARY_DIR}/app.rc" @ONLY)
add_executable(action75_paintball_controls_tests tests/paintball_controls_tests.cpp)
target_link_libraries(action75_paintball_controls_tests PRIVATE action75_settings action75_effekseer_backend d3d11 d3dcompiler dxgi ole32)
add_executable(action75_prepare_legacy_paintball EXCLUDE_FROM_ALL tests/PrepareLegacyPaintball.cpp)
target_include_directories(action75_prepare_legacy_paintball PRIVATE
    "${ACTION75_EFFEKSEER_ROOT}/src/EffekseerMaterialCompiler")
target_link_libraries(action75_prepare_legacy_paintball PRIVATE EffekseerRendererDX11)
set(paintball_artwork "${ACTION75_ROOT}/wallpapers-native/paintball-native")
set(paintball_abi1_fixture "${CMAKE_CURRENT_BINARY_DIR}/test-data/paintball-abi1")
file(GLOB paintball_test_effects CONFIGURE_DEPENDS "${paintball_artwork}/*.efk")
file(GLOB paintball_test_materials CONFIGURE_DEPENDS "${paintball_artwork}/materials/*.efkmat")
file(GLOB_RECURSE paintball_test_resources CONFIGURE_DEPENDS
    "${paintball_artwork}/textures/*")
set(paintball_ready "${paintball_abi1_fixture}/ready")
# Compile legacy caches in the test tree and leave the ABI2 release cache intact.
add_custom_command(OUTPUT "${paintball_ready}"
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${paintball_abi1_fixture}/materials"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
        "${paintball_artwork}/settings.json" ${paintball_test_effects} "${paintball_abi1_fixture}"
    COMMAND "${CMAKE_COMMAND}" -E copy_directory
        "${paintball_artwork}/textures" "${paintball_abi1_fixture}/textures"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
        ${paintball_test_materials} "${paintball_abi1_fixture}/materials"
    COMMAND action75_prepare_legacy_paintball "${paintball_artwork}" "${paintball_abi1_fixture}"
    COMMAND "${CMAKE_COMMAND}" -E touch "${paintball_ready}"
    DEPENDS action75_prepare_legacy_paintball "${paintball_artwork}/settings.json"
            ${paintball_test_effects} ${paintball_test_materials} ${paintball_test_resources}
    VERBATIM)
add_custom_target(action75_legacy_paintball_fixture DEPENDS "${paintball_ready}")
add_dependencies(action75_paintball_controls_tests action75_legacy_paintball_fixture)
add_test(NAME native_paintball_controls COMMAND action75_paintball_controls_tests "${paintball_abi1_fixture}")
add_library(action75_native_plugin STATIC src/NativePluginSession.cpp)
target_include_directories(action75_native_plugin PUBLIC include)
target_link_libraries(action75_native_plugin PUBLIC d3d11 dxgi advapi32 userenv user32 ole32)
add_executable(Action75WallpaperWorker WIN32 src/NativePluginWorker.cpp src/NativePluginWorker12.cpp cmake/ProcessHeap.manifest)
target_include_directories(Action75WallpaperWorker PRIVATE include)
target_link_libraries(Action75WallpaperWorker PRIVATE action75_json d3d11 d3d12 dxgi advapi32 shell32 ole32)
if(MSVC)
    target_link_libraries(Action75WallpaperWorker PRIVATE delayimp)
    target_link_options(Action75WallpaperWorker PRIVATE /DELAYLOAD:d3d12.dll /DELAYLOAD:d3d11.dll)
endif()
set_target_properties(Action75WallpaperWorker PROPERTIES RUNTIME_OUTPUT_DIRECTORY "$<1:${ACTION75_DIST_DIR}/Action75Native>")
foreach(host Action75Native Action75Perf)
    add_executable(${host} WIN32 cmake/ProcessHeap.manifest
        src/main.cpp src/IpcServer.cpp src/Dx12Output.cpp src/KeyboardTaskbar.cpp
        src/SpriteBurst.cpp src/KeycapEffect.cpp src/FieldEffect.cpp src/ProceduralBurst.cpp src/PixelFluid.cpp)
    target_include_directories(${host} PRIVATE include)
    target_link_libraries(${host} PRIVATE action75_pack_backdrop
        action75_input action75_settings action75_wallpaper_catalog action75_display_guard
        action75_effekseer_backend action75_water_surface action75_fireworks action75_glass_shatter
        action75_native_plugin d3d11 d3d12 dcomp dxgi d3dcompiler windowscodecs ole32 shell32 psapi advapi32
        mfplat mfuuid mf oleaut32 shlwapi)
    if(MSVC)
        target_link_libraries(${host} PRIVATE delayimp)
        target_link_options(${host} PRIVATE /DELAYLOAD:d3d11.dll /DELAYLOAD:d3d12.dll /DELAYLOAD:dcomp.dll)
    endif()
    set_target_properties(${host} PROPERTIES RUNTIME_OUTPUT_DIRECTORY "$<1:${ACTION75_DIST_DIR}/${host}>")
    add_dependencies(${host} Action75WallpaperWorker)
    target_sources(${host} PRIVATE src/TrayMenu.cpp)
    target_link_libraries(${host} PRIVATE shcore gdi32 uxtheme dwmapi wtsapi32)
endforeach()
target_sources(Action75Native PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/app.rc")
function(stage_action75_worker host)
    add_custom_target(${host}_stage_worker
        COMMAND ${CMAKE_COMMAND} -E make_directory "$<TARGET_FILE_DIR:${host}>"
        COMMAND ${CMAKE_COMMAND} -E copy_if_different "$<TARGET_FILE:Action75WallpaperWorker>"
            "$<TARGET_FILE_DIR:${host}>/Action75WallpaperWorker.exe"
        DEPENDS Action75WallpaperWorker VERBATIM)
    add_dependencies(${host} ${host}_stage_worker)
endfunction()
stage_action75_worker(Action75Perf)
add_executable(action75_native_core_tests tests/core_tests.cpp)
target_link_libraries(action75_native_core_tests PRIVATE action75_input action75_display_guard)
foreach(target Action75Native action75_input action75_settings action75_display_guard action75_water_surface action75_fireworks action75_glass_shatter action75_native_core_tests)
    target_compile_options(${target} PRIVATE $<$<CXX_COMPILER_ID:MSVC>:/W4>)
endforeach()
enable_testing()
add_library(action75_worker_test_wallpaper MODULE tests/WorkerTestWallpaper.cpp)
target_include_directories(action75_worker_test_wallpaper PRIVATE include)
target_link_libraries(action75_worker_test_wallpaper PRIVATE advapi32)
set_target_properties(action75_worker_test_wallpaper PROPERTIES LIBRARY_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/test-wallpaper")
add_executable(action75_native_plugin_tests tests/NativePluginSessionTests.cpp)
target_link_libraries(action75_native_plugin_tests PRIVATE action75_native_plugin)
add_dependencies(action75_native_plugin_tests Action75WallpaperWorker action75_worker_test_wallpaper)
stage_action75_worker(action75_native_plugin_tests)
add_test(NAME native_appcontainer_plugin COMMAND action75_native_plugin_tests "$<TARGET_FILE:action75_worker_test_wallpaper>")
add_executable(action75_wallpaper_catalog_tests tests/wallpaper_catalog_tests.cpp src/SpriteBurst.cpp)
target_link_libraries(action75_wallpaper_catalog_tests PRIVATE action75_wallpaper_catalog d3d11 d3dcompiler windowscodecs ole32)
add_test(NAME native_wallpaper_catalog COMMAND action75_wallpaper_catalog_tests)
add_executable(action75_pixel_fluid_tests tests/pixel_fluid_tests.cpp src/PixelFluid.cpp)
target_include_directories(action75_pixel_fluid_tests PRIVATE include)
target_compile_options(action75_pixel_fluid_tests PRIVATE $<$<CXX_COMPILER_ID:MSVC>:/UNDEBUG>)
add_test(NAME native_pixel_fluid_physics COMMAND action75_pixel_fluid_tests)
add_executable(action75_procedural_probe tests/ProceduralProbe.cpp src/ProceduralBurst.cpp src/PixelFluid.cpp src/SpriteBurst.cpp)
target_include_directories(action75_procedural_probe PRIVATE include)
target_link_libraries(action75_procedural_probe PRIVATE action75_pack_backdrop action75_settings action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_executable(action75_snow_probe tests/SnowCrystalProbe.cpp src/FieldEffect.cpp)
target_include_directories(action75_snow_probe PRIVATE include)
target_link_libraries(action75_snow_probe PRIVATE action75_pack_backdrop action75_settings action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_executable(action75_field_probe tests/FieldProbe.cpp src/FieldEffect.cpp)
target_include_directories(action75_field_probe PRIVATE include)
target_link_libraries(action75_field_probe PRIVATE action75_pack_backdrop action75_settings action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_executable(action75_keycap_probe tests/KeycapProbe.cpp src/KeycapEffect.cpp)
target_include_directories(action75_keycap_probe PRIVATE include)
target_link_libraries(action75_keycap_probe PRIVATE action75_pack_backdrop action75_settings action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_executable(action75_display_guard_tests tests/display_guard_policy_tests.cpp)
target_include_directories(action75_display_guard_tests PRIVATE include)
add_test(NAME native_display_guard_policy COMMAND action75_display_guard_tests)
add_executable(action75_pack_settings_tests tests/pack_settings_tests.cpp)
target_link_libraries(action75_pack_settings_tests PRIVATE action75_settings)
add_test(NAME native_pack_settings COMMAND action75_pack_settings_tests)
add_test(NAME native_mapping_input COMMAND action75_native_core_tests "${ACTION75_ROOT}")
add_executable(action75_effekseer_probe tests/effekseer-probe/BackendProbe.cpp)
target_link_libraries(action75_effekseer_probe PRIVATE action75_pack_backdrop action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_effect_pixel_mapping COMMAND action75_effekseer_probe "${ACTION75_EFFEKSEER_DIAGNOSTIC_RESOURCES}/square_r.efkefc")
add_subdirectory(tests/video-probe)


add_executable(action75_sprite_state_tests tests/sprite_state_tests.cpp)
target_include_directories(action75_sprite_state_tests PRIVATE include)
add_test(NAME native_sprite_state COMMAND action75_sprite_state_tests)
add_executable(action75_sprite_burst_probe tests/SpriteBurstProbe.cpp
    src/SpriteBurst.cpp)
target_include_directories(action75_sprite_burst_probe PRIVATE include)
target_link_libraries(action75_sprite_burst_probe PRIVATE action75_pack_backdrop action75_effekseer_backend
    d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_sprite_burst_pixels COMMAND action75_sprite_burst_probe
    "${ACTION75_ROOT}/wallpapers-native/afterimage/background.png"
    "${ACTION75_ROOT}/wallpapers-native/afterimage/burst.png")
add_test(NAME native_sprite_multiply_pixels COMMAND action75_sprite_burst_probe
    "${ACTION75_ROOT}/wallpapers-native/folio/background.png"
    "${ACTION75_ROOT}/wallpapers-native/folio/burst.png" --multiply)
add_test(NAME native_opaque_backdrop_copy COMMAND action75_sprite_burst_probe
    "${ACTION75_ROOT}/wallpapers-native/afterimage/background.png" --backdrop)

add_executable(action75_frame_pacer_tests tests/pacer_workload.cpp)
target_include_directories(action75_frame_pacer_tests PRIVATE include)
add_test(NAME native_frame_pacer_workload COMMAND action75_frame_pacer_tests)
add_executable(action75_frame_latency_tests tests/frame_latency_tests.cpp)
target_include_directories(action75_frame_latency_tests PRIVATE include)
add_test(NAME native_frame_latency_recovery COMMAND action75_frame_latency_tests)
add_test(NAME native_atlas_backend_parity COMMAND action75_sprite_burst_probe
    "${ACTION75_ROOT}/wallpapers-native/afterimage/background.png"
    "${ACTION75_ROOT}/wallpapers-native/afterimage/burst.png" --atlas-parity)
set_tests_properties(native_atlas_backend_parity PROPERTIES SKIP_RETURN_CODE 77)

add_executable(action75_background_appearance_tests tests/background_appearance_tests.cpp
    src/SpriteBurst.cpp)
target_include_directories(action75_background_appearance_tests PRIVATE include)
target_link_libraries(action75_background_appearance_tests PRIVATE action75_pack_backdrop action75_effekseer_backend
    d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_background_appearance_pixels COMMAND action75_background_appearance_tests)

set(black_hole_src "${ACTION75_ROOT}/examples/native-wallpapers/black-hole/src")
include("${black_hole_src}/../cmake/BlackHoleShaders.cmake")
add_executable(action75_black_hole_scene_tests tests/black_hole_scene_tests.cpp "${black_hole_src}/BlackHoleScene.cpp")
action75_black_hole_shaders(action75_black_hole_scene_tests)
target_include_directories(action75_black_hole_scene_tests PRIVATE "${black_hole_src}" include
    "${ACTION75_ROOT}/vendor-source/windows-native/nlohmann" "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(action75_black_hole_scene_tests PRIVATE action75_pack_backdrop
    action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_black_hole_scene COMMAND action75_black_hole_scene_tests
    "${ACTION75_ROOT}/wallpapers-native/black-hole")

add_executable(action75_fireworks_scene_tests tests/fireworks_scene_tests.cpp)
target_link_libraries(action75_fireworks_scene_tests PRIVATE action75_pack_backdrop action75_fireworks action75_settings
    action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_fireworks_scene COMMAND action75_fireworks_scene_tests
    "${ACTION75_ROOT}/wallpapers-native/night-fireworks")

add_executable(action75_water_surface_tests tests/water_surface_tests.cpp)
target_link_libraries(action75_water_surface_tests PRIVATE action75_pack_backdrop action75_water_surface action75_settings
    action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_water_surface COMMAND action75_water_surface_tests
    "${ACTION75_ROOT}/wallpapers-native/stillwater")

add_executable(action75_glass_shatter_tests tests/glass_shatter_tests.cpp)
target_link_libraries(action75_glass_shatter_tests PRIVATE action75_pack_backdrop action75_glass_shatter action75_settings
    action75_effekseer_backend d3d11 dxgi d3dcompiler windowscodecs ole32 mfplat mfuuid mf oleaut32 shlwapi)
add_test(NAME native_glass_shatter COMMAND action75_glass_shatter_tests
    "${ACTION75_ROOT}/wallpapers-native/glass-shatter")

add_executable(action75_diagnostics_tests tests/diagnostics_tests.cpp)
target_include_directories(action75_diagnostics_tests PRIVATE include)
target_link_libraries(action75_diagnostics_tests PRIVATE action75_json)
add_test(NAME native_diagnostics_retention COMMAND action75_diagnostics_tests)
add_executable(action75_surface_pack_parity tests/SurfacePackParity.cpp)
target_link_libraries(action75_surface_pack_parity PRIVATE action75_settings action75_water_surface action75_fireworks d3d11)
add_test(NAME native_surface_pack_parity COMMAND action75_surface_pack_parity "${ACTION75_ROOT}/tools/fixtures/builtins-abi1")

foreach(target Action75Native Action75Perf action75_snow_probe action75_field_probe action75_keycap_probe)
    action75_use_field_shaders(${target})
endforeach()

add_test(NAME native_monitor_reconnect COMMAND python
    "${CMAKE_CURRENT_SOURCE_DIR}/tests/monitor_reconnect.py" "$<TARGET_FILE:Action75Native>" "${ACTION75_ROOT}")
set_tests_properties(native_monitor_reconnect PROPERTIES TIMEOUT 45)

add_executable(action75_dx12_window_tests tests/Dx12WindowTest.cpp src/Dx12Output.cpp)
target_link_libraries(action75_dx12_window_tests PRIVATE action75_pack_backdrop d3d12 dcomp)
add_test(NAME native_dx12_window COMMAND action75_dx12_window_tests)
set_tests_properties(native_dx12_window PROPERTIES TIMEOUT 20)
add_test(NAME native_dx12_reconnect COMMAND python
    "${CMAKE_CURRENT_SOURCE_DIR}/tests/monitor_reconnect.py" "$<TARGET_FILE:Action75Native>" "${ACTION75_ROOT}" --dx12)
set_tests_properties(native_dx12_reconnect PROPERTIES TIMEOUT 45)

add_executable(action75_desktop_shell_fixture tests/DesktopShellFixture.cpp)
target_link_libraries(action75_desktop_shell_fixture PRIVATE user32)
add_test(NAME native_desktop_layer COMMAND python "${CMAKE_CURRENT_SOURCE_DIR}/tests/desktop_layer.py"
    "$<TARGET_FILE:Action75Native>" "$<TARGET_FILE:action75_desktop_shell_fixture>" "${ACTION75_ROOT}")
set_tests_properties(native_desktop_layer PROPERTIES TIMEOUT 70)
