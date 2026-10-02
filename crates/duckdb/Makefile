.PHONY: clean clean_all

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=mentat

# Unstable C API is currently REQUIRED by duckdb-rs. Consequence: the produced
# binary loads ONLY into TARGET_DUCKDB_VERSION (v1.5.6) — forward compat broken.
# (plan §1.2, §9 risk 1). Bumping DuckDB = bump the crate pin + this together.
USE_UNSTABLE_C_API=1

# Target DuckDB version. Must match the `duckdb = "~1.10506.0"` crate pin
# (= DuckDB v1.5.6) and the venv/PyPI `duckdb` used to test/load.
TARGET_DUCKDB_VERSION=v1.5.6

# The version stamped into the extension footer: the workspace version from the
# root Cargo.toml, so it can't drift from the release (this also works in copies
# without .git, where the ci-tools git-describe detection fails). The registry
# CI overrides it with its own EXTENSION_VERSION when it builds.
EXTENSION_VERSION ?= v$(shell sed -n 's/^version = "\(.*\)"/\1/p' $(PROJ_DIR)../../Cargo.toml | head -1)

# This crate is a workspace member, so `cargo build` writes to the WORKSPACE
# target dir (../../target), not ./target. Force a crate-local target dir so the
# ci-tools rust.Makefile finds the artifact at ./target/debug/lib<name>.so.
export CARGO_TARGET_DIR=$(PROJ_DIR)target

all: configure debug

# Include makefiles from DuckDB's extension-ci-tools submodule.
include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

# The cdylib package is `mentat_duckdb`, so cargo emits `libmentat_duckdb.{so,
# dylib}` (unix) or `mentat_duckdb.dll` (Windows, no `lib` prefix) -- NOT
# `lib$(EXTENSION_NAME).*`. (We can't name the lib `mentat`: it collides with
# the `mentat` path dependency in the crate graph.) Only RUST_LIBNAME -- the
# SOURCE artifact cargo produces -- needs overriding, and it is
# platform-specific. Do NOT override EXTENSION_LIB_FILENAME: base.Makefile
# already computes the correct per-platform copy DESTINATION name
# (libmentat.so / libmentat.dylib / mentat.dll), and the metadata step reads
# that same name -- overriding it to a fixed `.so` is exactly what broke the
# Windows and macOS builds (cargo never produced a `.so` there, so the copy
# failed with FileNotFoundError). The footer output stays
# `mentat.duckdb_extension` (driven separately by EXTENSION_NAME).
ifeq ($(OS),Windows_NT)
	RUST_LIBNAME=mentat_duckdb.dll
else ifeq ($(DUCKDB_PLATFORM),osx_amd64)
	RUST_LIBNAME=libmentat_duckdb.dylib
else ifeq ($(DUCKDB_PLATFORM),osx_arm64)
	RUST_LIBNAME=libmentat_duckdb.dylib
else ifeq ($(shell uname -s),Darwin)
	RUST_LIBNAME=libmentat_duckdb.dylib
else
	RUST_LIBNAME=libmentat_duckdb.so
endif

# A bare `cargo build` inside a workspace member dir does NOT reliably build
# THIS package (it builds the default-member set, which excludes mentat_duckdb).
# Force the package explicitly. TARGET_INFO is appended to the cargo invocation
# by rust.Makefile (included above), which sets it to `--target <triple>` for
# the macOS cross builds (osx_arm64/osx_amd64) and reads the artifact from
# ./target/<triple>/. APPEND, never assign: `TARGET_INFO=-p ...` dropped the
# --target, so cargo wrote ./target/release/ and the registry's macOS builds
# failed with FileNotFoundError on ./target/aarch64-apple-darwin/release/.
TARGET_INFO += -p mentat_duckdb

configure: venv platform extension_version

debug: build_extension_library_debug build_extension_with_metadata_debug
release: build_extension_library_release build_extension_with_metadata_release

test: test_debug
test_debug: test_extension_debug
test_release: test_extension_release

clean: clean_build clean_rust
clean_all: clean_configure clean
