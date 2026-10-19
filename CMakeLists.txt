### ZKGBAI (Rust) as a Recoil engine skirmish AI
#
# Drop this repository into a Recoil checkout as AI/Skirmish/ZKGBAI; the
# engine's AI build picks it up like the C++ AIs. Building requires `cargo`
# (Rust >= rust-version in Cargo.toml). Cross builds map the C toolchain's
# target to the matching Rust target triple.

set(myName "ZKGBAI")
set(myDir  "${CMAKE_CURRENT_SOURCE_DIR}")

if (NOT COMMAND get_version_plus_dep_file)
	message(FATAL_ERROR "ZKGBAI: build this as part of the Recoil engine (AI/Skirmish/ZKGBAI)")
endif ()

find_program(CARGO_EXECUTABLE cargo HINTS "$ENV{CARGO_HOME}/bin" "$ENV{HOME}/.cargo/bin")
if (NOT CARGO_EXECUTABLE OR myName MATCHES "${AI_EXCLUDE_REGEX}")
	message("warning: ${myName} Skirmish AI will not be built! (cargo not found or excluded)")
	return()
endif ()

get_version_plus_dep_file(myVersion myVersionDepFile)
set(myInstLibsDir "${SKIRMISH_AI_LIBS}/${myName}/${myVersion}")
set(myInstDataDir "${SKIRMISH_AI_DATA}/${myName}/${myVersion}")
skirmish_ai_message(STATUS "Found Skirmish AI: ${myName} ${myVersion} (Rust)")

if (WIN32)
	set(rustTarget "x86_64-pc-windows-gnu")
	set(rustLib    "zkgbai.dll")
	set(aiLib      "SkirmishAI.dll")
elseif (APPLE)
	set(rustTarget "")
	set(rustLib    "libzkgbai.dylib")
	set(aiLib      "libSkirmishAI.dylib")
else ()
	set(rustTarget "")
	set(rustLib    "libzkgbai.so")
	set(aiLib      "libSkirmishAI.so")
endif ()
if (DEFINED ZKGBAI_RUST_TARGET)
	set(rustTarget "${ZKGBAI_RUST_TARGET}")
endif ()

set(cargoTargetDir "${CMAKE_CURRENT_BINARY_DIR}/cargo")
set(cargoArgs build --release --locked -p zkgbai --target-dir "${cargoTargetDir}")
if (rustTarget)
	list(APPEND cargoArgs --target "${rustTarget}")
	set(builtLib "${cargoTargetDir}/${rustTarget}/release/${rustLib}")
else ()
	set(builtLib "${cargoTargetDir}/release/${rustLib}")
endif ()

add_custom_target(${myName} ALL
	COMMAND "${CARGO_EXECUTABLE}" ${cargoArgs}
	COMMAND "${CMAKE_COMMAND}" -E copy "${builtLib}" "${CMAKE_CURRENT_BINARY_DIR}/${aiLib}"
	WORKING_DIRECTORY "${myDir}"
	BYPRODUCTS "${CMAKE_CURRENT_BINARY_DIR}/${aiLib}"
	COMMENT "Building ${myName} with cargo"
	USES_TERMINAL
	VERBATIM)

file(READ "${myDir}/crates/zkgbai/data/AIInfo.lua" aiInfo)
string(REPLACE "@VERSION@" "${myVersion}" aiInfo "${aiInfo}")
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/AIInfo.lua" "${aiInfo}")

install(PROGRAMS "${CMAKE_CURRENT_BINARY_DIR}/${aiLib}" DESTINATION "${myInstLibsDir}")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/AIInfo.lua" "${myDir}/crates/zkgbai/data/AIOptions.lua" DESTINATION "${myInstLibsDir}")
