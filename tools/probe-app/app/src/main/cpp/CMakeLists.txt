cmake_minimum_required(VERSION 3.22.1)
project(qprofprobe C)

add_library(probe SHARED probe.c)

target_compile_options(probe PRIVATE -O2 -Wall)
target_include_directories(probe PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(probe android log vulkan dl)
