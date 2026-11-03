cmake_minimum_required(VERSION 3.22)

project(michost_juce_host LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

option(MICHOST_ASIO "Enable the ASIO device type (requires the Steinberg ASIO SDK)" ON)
set(ASIO_SDK_DIR "" CACHE PATH "Path to the Steinberg ASIO SDK (contains common/)")
set(MICHOST_JUCE_DIR "" CACHE PATH "Existing JUCE checkout; fetched from GitHub when empty")
set(MICHOST_RUST_TARGET_DIR "" CACHE PATH "Rust target directory to copy mic_host.dll into")

if (MICHOST_JUCE_DIR)
    add_subdirectory(${MICHOST_JUCE_DIR} JUCE)
else()
    include(FetchContent)
    FetchContent_Declare(JUCE
        GIT_REPOSITORY https://github.com/juce-framework/JUCE.git
        GIT_TAG 8.0.7
        GIT_SHALLOW TRUE
        GIT_PROGRESS TRUE)
    FetchContent_MakeAvailable(JUCE)
endif()

add_library(mic_host SHARED
    src/api.cpp
    src/device.cpp
    src/editors.cpp
    src/plugins.cpp
    src/scan.cpp)

target_include_directories(mic_host PRIVATE include)

target_link_libraries(mic_host PRIVATE
    juce::juce_core
    juce::juce_events
    juce::juce_audio_basics
    juce::juce_audio_devices
    juce::juce_audio_processors
    juce::juce_gui_basics
    juce::juce_audio_utils
    juce::juce_recommended_warning_flags)

target_compile_definitions(mic_host PRIVATE
    MH_BUILD_DLL=1
    JUCE_PLUGINHOST_VST3=1
    JUCE_WEB_BROWSER=0
    JUCE_USE_CURL=0
    JUCE_MODAL_LOOPS_PERMITTED=1
    JUCE_STRICT_REFCOUNTEDPOINTER=1
    JUCE_STANDALONE_APPLICATION=0)

if (MSVC)
    target_compile_options(mic_host PRIVATE /EHsc /utf-8)
    set_target_properties(mic_host PROPERTIES
        PDB_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
endif()

if (MICHOST_ASIO)
    if (NOT ASIO_SDK_DIR)
        message(FATAL_ERROR "MICHOST_ASIO=ON requires -DASIO_SDK_DIR=<path to ASIO SDK>")
    endif()
    target_compile_definitions(mic_host PRIVATE JUCE_ASIO=1)
    target_include_directories(mic_host PRIVATE "${ASIO_SDK_DIR}/common")
    target_link_libraries(mic_host PRIVATE ole32 oleaut32 uuid)
endif()

if (MICHOST_RUST_TARGET_DIR)
    add_custom_command(TARGET mic_host POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
            $<TARGET_FILE:mic_host>
            "${MICHOST_RUST_TARGET_DIR}/mic_host.dll"
        COMMENT "Copying mic_host.dll to ${MICHOST_RUST_TARGET_DIR}")
    if (MSVC)
        add_custom_command(TARGET mic_host POST_BUILD
            COMMAND ${CMAKE_COMMAND}
                "-DSRC=$<TARGET_PDB_FILE:mic_host>"
                "-DDST=${MICHOST_RUST_TARGET_DIR}/mic_host.pdb"
                -P "${CMAKE_CURRENT_SOURCE_DIR}/cmake/copy_pdb.cmake"
            COMMENT "Copying mic_host.pdb to ${MICHOST_RUST_TARGET_DIR}")
    endif()
endif()
