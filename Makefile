# Merged mentat repository Makefile.
#
# SQLite side (default `cargo` targets, no postgres):
#   make test              cargo test (default members)
#   make outdated / fix    dependency maintenance
# PostgreSQL side (needs pgrx + pg_config):
#   make test-pg [PG=16]   cargo pgrx test
#   make package-pg PG=16  cargo pgrx package
#   make install-pg        cargo pgrx install
#   make upgrades / install-upgrade-scripts / smoke

.PHONY: test test-pg package-pg install-pg \
        outdated fix upgrades install-upgrade-scripts smoke

# pgrx cargo feature / PostgreSQL major to operate on (override: make test-pg PG=17).
PG ?= 16
PG_MENTAT_DIR := crates/pg/pg_mentat

# PostgreSQL extension directory (auto-detected via pg_config).
PG_SHAREDIR := $(shell pg_config --sharedir 2>/dev/null || echo "/usr/share/postgresql")
EXTENSION_DIR := $(PG_SHAREDIR)/extension

# --- SQLite side --------------------------------------------------------
test:
	cargo test

# --- PostgreSQL side ----------------------------------------------------
test-pg:
	cd $(PG_MENTAT_DIR) && cargo pgrx test --no-default-features --features pg$(PG) pg$(PG)

package-pg:
	cd $(PG_MENTAT_DIR) && cargo pgrx package --no-default-features --features pg$(PG)

install-pg:
	cd $(PG_MENTAT_DIR) && cargo pgrx install --release --no-default-features --features pg$(PG)

upgrades:
	cargo upgrades

# Install upgrade SQL scripts to the PostgreSQL extension directory.
# Run after `make install-pg` to enable ALTER EXTENSION ... UPDATE.
install-upgrade-scripts:
	@echo "Installing upgrade scripts to $(EXTENSION_DIR)"
	@for f in $(PG_MENTAT_DIR)/sql/upgrade--*.sql; do \
		target="$(EXTENSION_DIR)/pg_mentat--$$(echo $$f | sed 's|.*upgrade--||')"; \
		echo "  $$f -> $$target"; \
		install -m 644 "$$f" "$$target"; \
	done
	@echo "Done. Available upgrade paths:"
	@ls -1 $(EXTENSION_DIR)/pg_mentat--*--*.sql 2>/dev/null || echo "  (none installed)"

# Install pg_mentat into the pgrx-managed cluster and run the smoke-test SQL.
smoke:
	@bash scripts/pg/smoke.sh

# --- Maintenance --------------------------------------------------------
outdated:
	for p in $$(dirname $$(ls Cargo.toml */Cargo.toml */*/Cargo.toml)); do echo $$p; (cd $$p; cargo outdated -R); done

fix:
	$$(for p in $$(dirname $$(ls Cargo.toml */Cargo.toml */*/Cargo.toml)); do echo $$p; (cd $$p; cargo fix --allow-dirty --broken-code --edition-idioms); done)

# --- DuckDB extension (crates/duckdb) ------------------------------------
# The DuckDB Community Extensions CI (duckdb/community-extensions ->
# extension-ci-tools/_extension_distribution.yml) clones THIS repo at its root and
# runs `make configure_ci`, `make release` / `make debug`, `make test_release` /
# `make test_debug` here, then collects
#   build/<type>/extension/mentat/mentat.duckdb_extension
# from the repo root. The extension lives in crates/duckdb, so these targets
# forward there and copy the artifacts to where the registry looks. The registry
# also checks out its own extension-ci-tools at ./extension-ci-tools; we use the
# pinned submodule under crates/duckdb either way.
DUCKDB_DIR := crates/duckdb
.PHONY: configure configure_ci debug release test_debug test_release \
        set_duckdb_version set_duckdb_tag set_duckdb_repository

configure configure_ci:
	$(MAKE) -C $(DUCKDB_DIR) $@

debug release:
	$(MAKE) -C $(DUCKDB_DIR) $@
	mkdir -p build/$@/extension/mentat
	cp $(DUCKDB_DIR)/build/$@/mentat.duckdb_extension build/$@/
	cp $(DUCKDB_DIR)/build/$@/mentat.duckdb_extension build/$@/extension/mentat/

test_debug test_release:
	$(MAKE) -C $(DUCKDB_DIR) $@

# The registry sets these to pin the DuckDB it builds against; our extension is
# already pinned (TARGET_DUCKDB_VERSION in crates/duckdb/Makefile), so they're
# accepted and ignored.
set_duckdb_version set_duckdb_tag set_duckdb_repository:
	@echo "mentat: $@ is a no-op (DuckDB version pinned in $(DUCKDB_DIR)/Makefile)"
