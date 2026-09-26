.PHONY: clean clean_all

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=mentat

# Unstable C API is currently REQUIRED by duckdb-rs. Consequence: the produced
# binary loads ONLY into TARGET_DUCKDB_VERSION (v1.5.5) — forward compat broken.
# (plan §1.2, §9 risk 1). Bumping DuckDB = bump the crate pin + this together.
USE_UNSTABLE_C_API=1

# Target DuckDB version. Must match the `duckdb = "~1.10505.0"` crate pin
# (= DuckDB v1.5.5) and the venv/PyPI `duckdb` used to test/load.
TARGET_DUCKDB_VERSION=v1.5.5

# EC2's ~/mentat is not a git checkout (rsync excludes .git), so the auto
# git-describe version detection fails. Pin it explicitly.
EXTENSION_VERSION=v1.7.0

# This crate is a workspace member, so `cargo build` writes to the WORKSPACE
# target dir (../../target), not ./target. Force a crate-local target dir so the
# ci-tools rust.Makefile finds the artifact at ./target/debug/lib<name>.so.
export CARGO_TARGET_DIR=$(PROJ_DIR)target

all: configure debug

# Include makefiles from DuckDB's extension-ci-tools submodule.
include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

# The cdylib package is `mentat_duckdb`, so cargo emits `libmentat_duckdb.so`,
# not `lib$(EXTENSION_NAME).so`. (We can't name the lib `mentat` — it collides
# with the `mentat` path dependency in the crate graph.) Override the expected
# raw-library filename; the footer output is still `mentat.duckdb_extension`
# (driven separately by EXTENSION_NAME).
EXTENSION_LIB_FILENAME=libmentat_duckdb.so
RUST_LIBNAME=libmentat_duckdb.so

# A bare `cargo build` inside a workspace member dir does NOT reliably build
# THIS package (it builds the default-member set, which excludes mentat_duckdb).
# Force the package explicitly. TARGET_INFO is appended to the cargo invocation
# by rust.Makefile (empty on native linux), so this is the override hook.
TARGET_INFO=-p mentat_duckdb

configure: venv platform extension_version

debug: build_extension_library_debug build_extension_with_metadata_debug
release: build_extension_library_release build_extension_with_metadata_release

test: test_debug
test_debug: test_extension_debug
test_release: test_extension_release

clean: clean_build clean_rust
clean_all: clean_configure clean
