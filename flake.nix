{
  # Merged mentat repository: one flake builds either backend.
  #   nix build .#mentat-cli        # embedded SQLite CLI (crates/sqlite/cli)
  #   nix build .#mentatd           # HTTP server (crates/pg/mentatd)
  #   nix build .#pg_mentat-pg16    # PostgreSQL extension (crates/pg/pg_mentat)
  #   nix develop                   # full pgrx dev shell (LLVM/clang/postgres)
  #   nix develop .#sqlite          # lean SQLite-only shell (no clang/postgres)
  #
  # ponytail: not nix-verified in Task 6 (CI host is a rustup box, no nix on
  # EC2); paths updated for the crates/ layout by inspection + `nix flake check
  # --no-build` on the floki dev box.
  description = "mentat - Datomic-like store on SQLite, and pg_mentat PostgreSQL extension";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Rust toolchain. Pinned to 1.90.0 to match rust-toolchain.toml and the
        # CI host (EC2 rustup is 1.90). Plan §1.14 intends 1.98 eventually;
        # bump both here and in rust-toolchain.toml together when the build
        # host updates. MSRV floor stays rust-version = "1.88" in Cargo.toml.
        rustToolchain = pkgs.rust-bin.stable."1.90.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };

        # PostgreSQL version used for development/testing
        postgresql = pkgs.postgresql_16;

        # Build inputs required for pg_mentat and cargo-pgrx
        commonBuildInputs = with pkgs; [
          # Rust toolchain
          rustToolchain

          # Build tools
          pkg-config
          git

          # LLVM and Clang (required for pgrx bindgen)
          llvmPackages_18.libllvm
          llvmPackages_18.clang
          llvmPackages_18.libclang
          llvmPackages_18.lld  # LLVM linker

          # OpenSSL
          openssl
          openssl.dev

          # Other dependencies (include .dev outputs so a from-source
          # PostgreSQL build via `cargo pgrx init --pg16 download` finds the
          # readline/zlib/icu headers).
          zlib
          zlib.dev
          readline
          readline.dev
          icu
          icu.dev
          gettext  # NLS support

          # Build essentials
          gnumake
          gcc
          perl
        ];

        # Additional packages for the dev shell (not needed for pure builds)
        devOnlyInputs = with pkgs; [
          postgresql
          # pg_config is a separate derivation (postgresql.pg_config) in
          # current nixpkgs, not in the default or .dev output; pgrx needs it
          # on PATH for `cargo pgrx init/package/install`.
          postgresql.pg_config
          bison
          flex
        ];

        # pkg-config search path
        pkgConfigPath = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" [
          pkgs.openssl
          pkgs.zlib
          pkgs.readline
          pkgs.icu
        ];

        # Environment variables shared between devShell and derivations
        buildEnv = {
          LIBCLANG_PATH = "${pkgs.llvmPackages_18.libclang.lib}/lib";
          LLVM_CONFIG_PATH = "${pkgs.llvmPackages_18.libllvm.dev}/bin/llvm-config";
          # Critical for bindgen: tell it where to find C standard library headers
          # Use stdenv.cc.libc which has the complete glibc setup
          BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include -isystem ${pkgs.llvmPackages_18.libclang.lib}/lib/clang/18/include";
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.llvmPackages_18.libllvm
            pkgs.llvmPackages_18.libclang
            pkgs.openssl
            pkgs.zlib
            pkgs.readline
            pkgs.icu
          ];
          PKG_CONFIG_PATH = pkgConfigPath;
        };

        # Extra environment variables for the dev shell only
        devEnv = buildEnv // {
          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          CARGO_HOME = "${toString ./.}/.cargo";
          PGDATA = "${toString ./.}/.postgres-data";
        };

        # Build the installable extension against a specific PostgreSQL major.
        # Produces a PGXS-style layout (mirrors pg_fts):
        #   $out/lib/pg_mentat.so
        #   $out/share/postgresql/extension/pg_mentat.control
        #   $out/share/postgresql/extension/pg_mentat--<ver>.sql (+ upgrade sql)
        #
        # pgFeature is the pgrx cargo feature ("pg16"/"pg17"/"pg18"); pgPkg is the
        # matching nixpkgs postgresql derivation. We seed a writable PGRX_HOME
        # config so `cargo pgrx package` never calls the network-dependent
        # `cargo pgrx init` (which tries to download its own PostgreSQL and
        # write to a read-only ~/.pgrx -> EACCES in a Nix build).
        mkPgMentatExtension = { pgPkg, pgFeature }:
          pkgs.stdenv.mkDerivation {
            pname = "pg_mentat-${pgFeature}";
            version = extVersion;
            src = ./.;

            nativeBuildInputs = commonBuildInputs ++ [ pgPkg pgPkg.pg_config ];

            inherit (buildEnv)
              LIBCLANG_PATH LLVM_CONFIG_PATH BINDGEN_EXTRA_CLANG_ARGS
              LD_LIBRARY_PATH PKG_CONFIG_PATH;

            # cargo fetches crates from the network; a fully-sandboxed build
            # would need a vendored cargoHash. Until then this derivation is
            # impure (build with `--option sandbox relaxed` or a fixed-output
            # vendor). It never calls `cargo pgrx init`, which was the
            # reported blocker.
            __noChroot = true;

            buildPhase = ''
              runHook preBuild
              export CARGO_HOME=$(mktemp -d)
              export PGRX_HOME=$(mktemp -d)
              # Seed PGRX_HOME so cargo-pgrx maps the feature to this PG's
              # pg_config WITHOUT running `cargo pgrx init`.
              printf '[configs]\n${pgFeature} = "%s"\n' \
                "${pgPkg.pg_config}/bin/pg_config" > "$PGRX_HOME/config.toml"
              cargo install --locked cargo-pgrx --version '~0.17' --root "$CARGO_HOME/pgrx-tools"
              export PATH="$CARGO_HOME/pgrx-tools/bin:$PATH"
              (cd crates/pg/pg_mentat && cargo pgrx package \
                --no-default-features --features ${pgFeature} \
                --pg-config "${pgPkg.pg_config}/bin/pg_config" \
                --out-dir "$PWD/pgrx-out")
              runHook postBuild
            '';

            installPhase = ''
              runHook preInstall
              mkdir -p $out/lib $out/share/postgresql/extension
              # cargo pgrx package (run in the crates/pg/pg_mentat subdir)
              # writes a pg_config-relative tree under
              # crates/pg/pg_mentat/pgrx-out/nix/store/.../; grab the .so, the
              # generated base + upgrade SQL, and the control file from there.
              find crates/pg/pg_mentat/pgrx-out -name 'pg_mentat.so'      -exec cp {} $out/lib/ \;
              find crates/pg/pg_mentat/pgrx-out -name 'pg_mentat.control' -exec cp {} $out/share/postgresql/extension/ \;
              find crates/pg/pg_mentat/pgrx-out -name 'pg_mentat--*.sql'  -exec cp {} $out/share/postgresql/extension/ \;
              # Fall back to the source control file if not found in the package.
              if [ ! -f $out/share/postgresql/extension/pg_mentat.control ]; then
                cp crates/pg/pg_mentat/pg_mentat.control $out/share/postgresql/extension/
              fi
              test -f $out/lib/pg_mentat.so || (echo "ERROR: pg_mentat.so not produced" >&2; exit 1)
              runHook postInstall
            '';

            meta = with pkgs.lib; {
              description = "pg_mentat — Datomic-compatible Datalog engine for PostgreSQL (${pgFeature})";
              homepage = "https://codeberg.org/gregburd/mentat";
              license = licenses.asl20;
              platforms = platforms.linux;
            };
          };

        # Extension version, read from crates/pg/pg_mentat/pg_mentat.control's
        # default_version so it can never drift from the extension it builds
        # (the derivation .name/.version then always matches CREATE EXTENSION).
        extVersion =
          let
            m = builtins.match ".*default_version = '([0-9][0-9.]*)'.*"
              (builtins.readFile ./crates/pg/pg_mentat/pg_mentat.control);
          in
          if m == null then
            throw "flake.nix: could not read default_version from crates/pg/pg_mentat/pg_mentat.control"
          else
            builtins.elemAt m 0;

        # Cargo package version, read from [workspace.package] in Cargo.toml
        # (single source of truth; never a hardcoded literal that can drift).
        cargoVersion =
          (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        # Rust binaries built with cargo from the workspace. Both are the
        # SQLite/HTTP side (no pgrx), so they only need the rust toolchain and
        # the rusqlite/openssl build deps -- not clang/postgres. Impure fetch
        # (no vendored cargoHash yet), same tradeoff as the extension above.
        mkCargoBin = { pname, cargoPkg }:
          pkgs.stdenv.mkDerivation {
            inherit pname;
            version = cargoVersion;
            src = ./.;
            nativeBuildInputs = with pkgs; [ rustToolchain pkg-config ];
            buildInputs = with pkgs; [ openssl openssl.dev zlib ];
            inherit (buildEnv) PKG_CONFIG_PATH;
            __noChroot = true;
            buildPhase = ''
              runHook preBuild
              export CARGO_HOME=$(mktemp -d)
              cargo build --release --locked -p ${cargoPkg}
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p $out/bin
              # cargo bin name may differ from the crate name; copy any release
              # binary that matches the expected bin target.
              for b in ${pname} ${cargoPkg}; do
                if [ -x "target/release/$b" ]; then cp "target/release/$b" $out/bin/; fi
              done
              test -n "$(ls -A $out/bin 2>/dev/null)" || (echo "ERROR: no binary produced for ${pname}" >&2; exit 1)
              runHook postInstall
            '';
            meta = with pkgs.lib; {
              description = "${pname} (mentat merged repo)";
              homepage = "https://codeberg.org/gregburd/mentat";
              license = licenses.asl20;
              platforms = platforms.linux;
            };
          };

      in
      {
        # Development shells:
        #   default / pg   -> full pgrx toolchain (LLVM/clang/postgres)
        #   sqlite         -> lean: rust + rusqlite build deps only
        devShells.sqlite = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            pkg-config
            git
            openssl
            openssl.dev
            zlib
            zlib.dev
            sqlite
          ];
          inherit (buildEnv) PKG_CONFIG_PATH;
          shellHook = ''
            export CARGO_HOME="$PWD/.cargo"
            mkdir -p "$CARGO_HOME"
            echo "mentat SQLite dev shell (no clang/postgres)"
            echo "Rust:  $(rustc --version)"
            echo "Build the CLI:  cargo build -p mentat_cli"
            echo "Test SQLite:    cargo test"
          '';
        };

        # Development shell (full pgrx)
        devShells.default = pkgs.mkShell {
          buildInputs = commonBuildInputs ++ devOnlyInputs;

          shellHook = ''
            ${pkgs.lib.concatStringsSep "\n"
              (pkgs.lib.mapAttrsToList (name: value: "export ${name}=\"${toString value}\"") devEnv)}

            # CARGO_HOME / PGDATA from devEnv resolve to the nix-store copy of
            # the flake source, which is read-only -- cargo install then fails
            # with EACCES. Repoint them at the real working directory at shell
            # entry so they are writable (CI checks out into a writable dir).
            export CARGO_HOME="$PWD/.cargo"
            export PGDATA="$PWD/.postgres-data"

            # Create cargo home directory
            mkdir -p "$CARGO_HOME"

            # Helper: install and initialize cargo-pgrx
            setup-pgrx() {
              echo "Installing cargo-pgrx 0.17..."
              cargo install --locked cargo-pgrx --version '~0.17'
              echo "Initializing pgrx with the dev-shell PostgreSQL 16..."
              cargo pgrx init --pg16="${postgresql.pg_config}/bin/pg_config"
              echo "pgrx setup complete."
            }

            # Helper: run tests against PostgreSQL 16. cargo pgrx test takes
            # only [PG_VERSION] [TESTNAME] + options; let it regenerate the
            # schema (the canonical GitHub ci job does the same).
            test-pg16() {
              (cd crates/pg/pg_mentat && cargo pgrx test --no-default-features --features pg16 pg16 "$@")
            }

            # Helper: build extension in release mode
            build-extension() {
              (cd crates/pg/pg_mentat && cargo pgrx package --pg-config="${postgresql.pg_config}/bin/pg_config")
            }

            # Helper: install extension to local PostgreSQL
            install-extension() {
              (cd crates/pg/pg_mentat && cargo pgrx install --release --pg-config="${postgresql.pg_config}/bin/pg_config")
            }

            # Helper: start a local PostgreSQL instance
            start-postgres() {
              if [ ! -d "$PGDATA" ]; then
                echo "Initializing PostgreSQL data directory at $PGDATA..."
                initdb -D "$PGDATA" --no-locale --encoding=UTF8
              fi
              echo "Starting PostgreSQL..."
              pg_ctl -D "$PGDATA" -l "$PGDATA/server.log" start
              echo "PostgreSQL running. Stop with: pg_ctl -D \"$PGDATA\" stop"
            }

            # Welcome message
            echo "pg_mentat development environment"
            echo ""
            echo "Rust:       $(rustc --version)"
            echo "Cargo:      $(cargo --version)"
            echo "PostgreSQL: $(pg_config --version)"
            echo ""
            echo "Environment:"
            echo "  CARGO_HOME=$CARGO_HOME"
            echo ""
            echo "Commands:"
            echo "  setup-pgrx          Install and initialize cargo-pgrx"
            echo "  test-pg16 [args]    Run tests against PostgreSQL 16"
            echo "  build-extension     Package the extension"
            echo "  install-extension   Install to local PostgreSQL"
            echo "  start-postgres      Start a local PostgreSQL instance"
            echo ""

            # Export the helper functions so they are available in
            # `nix develop --command bash -c '...'` child shells (used by CI).
            export -f setup-pgrx test-pg16 build-extension install-extension start-postgres
          '';
        };

        # Alias: `nix develop .#pg` is the same full pgrx shell as default.
        devShells.pg = self.devShells.${system}.default;

        # Build outputs. The extension has one output per PG major; the two
        # cargo binaries are the SQLite/HTTP side.
        #   nix build .#mentat-cli .#mentatd .#pg_mentat-pg16
        packages = {
          default = self.packages.${system}.pg_mentat-pg16;

          # Per-PG-major extension builds (PGXS layout, mirrors pg_fts).
          # nixpkgs-unstable dropped postgresql_13 (EOL upstream); pg13 is still
          # built + tested in the Forgejo/GitHub CI matrices via apt/pgrx.
          pg_mentat-pg14 = mkPgMentatExtension { pgPkg = pkgs.postgresql_14; pgFeature = "pg14"; };
          pg_mentat-pg15 = mkPgMentatExtension { pgPkg = pkgs.postgresql_15; pgFeature = "pg15"; };
          pg_mentat-pg16 = mkPgMentatExtension { pgPkg = pkgs.postgresql_16; pgFeature = "pg16"; };
          pg_mentat-pg17 = mkPgMentatExtension { pgPkg = pkgs.postgresql_17; pgFeature = "pg17"; };
          pg_mentat-pg18 = mkPgMentatExtension { pgPkg = pkgs.postgresql_18; pgFeature = "pg18"; };

          # SQLite embedded CLI (crates/sqlite/cli, crate `mentat_cli`).
          mentat-cli = mkCargoBin { pname = "mentat-cli"; cargoPkg = "mentat_cli"; };

          # HTTP server (crates/pg/mentatd, crate `mentatd`).
          mentatd = mkCargoBin { pname = "mentatd"; cargoPkg = "mentatd"; };

          # Back-compat aliases for the previous output names.
          pg16 = self.packages.${system}.pg_mentat-pg16;
          pg_mentat = self.packages.${system}.pg_mentat-pg16;
        };

        # Checks for CI/CD -- validate the extension builds against each major.
        checks = {
          pg16 = self.packages.${system}.pg_mentat-pg16;
          pg17 = self.packages.${system}.pg_mentat-pg17;
          pg18 = self.packages.${system}.pg_mentat-pg18;

          # Guard the version single-source-of-truth: the pg_mentat control
          # file's default_version (which drives extVersion) and the Cargo
          # workspace version (cargoVersion) must agree. Catches a release that
          # bumps one but not the other, and a malformed control file
          # (extVersion would otherwise throw at eval). Trivial build-only derivation.
          version-sync =
            pkgs.runCommand "mentat-version-sync"
              {
                inherit extVersion cargoVersion;
              } ''
              if [ "$extVersion" != "$cargoVersion" ]; then
                echo "version drift: pg_mentat.control default_version ($extVersion)" >&2
                echo "does not match Cargo workspace version ($cargoVersion)." >&2
                echo "Bump both together." >&2
                exit 1
              fi
              echo "version-sync ok: $extVersion" > $out
            '';
        };
      }
    );
}
