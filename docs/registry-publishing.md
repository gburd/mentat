# Registry publishing: DuckDB Community Extensions + PGXN

Research report (verified 2025‑12, against the actual upstream repos). Goal:
automate, on a `v*` tag push, publishing `mentat_duckdb` to the DuckDB Community
Extensions registry and `pg_mentat` to PGXN — on top of the existing
`.github/workflows/release.yml` (which builds per‑PG tarballs + CLI and makes a
GitHub Release).

Repo: `codeberg.org/gregburd/mentat` (push‑mirrors to `github.com/gburd/mentat`).
CI runs on the GitHub side.

Two truths up front:

- **DuckDB registry:** publishing = **a PR to `duckdb/community-extensions`** that
  edits one `description.yml`. Their CI builds+signs centrally for all platforms.
  First submission is a human‑reviewed PR; version bumps can be a CI‑opened PR,
  but every publish still passes through their PR merge (a human gate).
- **PGXN:** publishing = `pgxn-bundle` + `pgxn-release` inside the
  `pgxn/pgxn-tools` container with `PGXN_USERNAME`/`PGXN_PASSWORD`. Fully
  automatable, **but** our `META.json` is currently wrong (bad `file` path,
  stale version) and the pgrx layout is non‑standard for classic PGXN install.
  Bundle will *upload* but the distribution is not source‑installable the
  classic way. Details below.

---

## 1. DuckDB Community Extensions — `mentat_duckdb`

Upstream verified:
- Repo: `https://github.com/duckdb/community-extensions`
- Submission model doc: `UPDATING.md` in that repo (Community Extension Update Guide).
- Rust extension template: `https://github.com/duckdb/extension-template-rs`
- CI‑tools (reusable build workflow + Makefiles): `https://github.com/duckdb/extension-ci-tools`

### 1.1 The submission model — one `description.yml` per extension

Register by adding **`extensions/<name>/description.yml`** (note: `.yml`, not
`.yaml`) to `duckdb/community-extensions` via PR. There is exactly one descriptor
file per extension; it points at *your* repo + a git ref. Their CI reads it,
clones your repo at that ref, builds, signs, and deploys.

Fields, verified against `scripts/build.py` and real descriptors:

```yaml
extension:
  name: mentat                    # INSTALL/LOAD name; must match Makefile EXTENSION_NAME
  description: <one line>          # required
  version: <string>               # your extension version (free-form; e.g. 1.8.0)
  language: Rust                   # display language
  build: cargo                    # build system: cmake | cargo (cargo => Rust template path)
  license: Apache-2.0
  maintainers:
    - gregburd                    # GitHub usernames
  requires_toolchains: "rust;python3"   # extra toolchains their CI installs before build
  # optional:
  # excluded_platforms: "wasm_mvp;wasm_eh;wasm_threads;windows_amd64_mingw;linux_amd64_musl"
  # vcpkg_url / vcpkg_commit      # only for C/C++ vcpkg deps; N/A for us

repo:
  github: gburd/mentat            # owner/repo that CI clones (the GitHub mirror)
  ref: <full 40-char commit SHA>  # the source ref they build (SHA, not tag — see 1.2)
  # ref_next: <sha>               # only used when validating against next DuckDB (see 1.2)

docs:
  hello_world: |                  # optional, shown on the website
    LOAD mentat;
    SELECT * FROM mentat_query('...');
  extended_description: |         # optional
    ...
```

Note what the descriptor **does not** contain: no `duckdb` version and no
`extension-ci-tools` version. Those are **owned by community‑extensions' own CI**,
not by you (see 1.3). You only declare `build: cargo` + `requires_toolchains: rust`
and point at your repo.

**Real pure‑Rust example** (verified live) —
`extensions/quackformers/description.yml`:

```yaml
extension:
  name: quackformers
  description: Bert-based embedding extension.
  version: 0.1.5.5
  language: Rust
  build: cargo
  license: MIT
  excluded_platforms: "wasm_mvp;wasm_eh;wasm_threads;windows_amd64_mingw;linux_amd64_musl"
  requires_toolchains: "rust;python3"
  maintainers:
    - martin-conur
repo:
  github: martin-conur/quackformers
  ref: 4741f1a317837cd110e5801825ad7af1bbc3bd87
docs:
  hello_world: |
    SELECT embed('this is an embeddable sentence');
```

`quackformers` is built from `duckdb/extension-template-rs` — the same base our
`crates/duckdb` Makefile mirrors. **That's our template.** (Most `query-farm`
Rust extensions such as `crypto`, `lindel`, `evalexpr_rhai` use
`build: cmake` + `requires_toolchains: rust` because they wrap Rust inside a C++
shell; the pure duckdb‑rs path is `build: cargo`, which is what we want.)

### 1.2 How new versions publish

From `UPDATING.md` and `scripts/build.py`:

- The descriptor points at **one active source ref** via `repo.ref`.
- **Updating `repo.ref` (and `extension.version`) and merging that PR** makes the
  new ref the source; community CI rebuilds and re‑deploys for every platform +
  DuckDB version. **Every release = a PR to `duckdb/community-extensions`.**
  There is no webhook/API that pulls your new tag automatically.
- `repo.ref` is a **commit SHA**, not a tag, in practice (all examples use SHAs).
  You can resolve `v1.8.0` → its SHA in CI and write the SHA.
- Community extensions are **also** rebuilt automatically whenever DuckDB itself
  releases a new version — you don't PR for that; it just re‑runs against the
  ref you already have. `ref_next` exists only to let you supply a *different*
  commit for validating against the upcoming (not-yet-stable) DuckDB.

So "automate future releases update the registry" concretely means:
**CI opens a PR to `duckdb/community-extensions` that changes exactly
`extensions/mentat/description.yml` — bumping `extension.version` and `repo.ref`
to the new tag's commit.**

### 1.3 Build & signing are central — version‑lock is a lucky match

Their CI does the build + sign for all platforms and all supported DuckDB
versions. You provide only a buildable repo at `repo.ref`. Verified from
`community-extensions/.github/workflows/build.yml`:

```
DUCKDB_LATEST_STABLE: 'v1.5.5'
DUCKDB_VERSION:       v1.5.5           # default
uses: duckdb/extension-ci-tools/.github/workflows/_extension_distribution.yml@v1.5-variegata
  duckdb_version:    v1.5.5
  ci_tools_version:  v1.5-variegata
  extra_toolchains:  <requires_toolchains from descriptor>
```

**This is the key alignment:** community‑extensions currently builds against
**DuckDB v1.5.5**, which is *exactly* what `mentat_duckdb` is pinned to
(`duckdb = "~1.10505.0"`, `TARGET_DUCKDB_VERSION=v1.5.5`, `USE_UNSTABLE_C_API=1`).
Our version‑lock is not a blocker right now — it matches the registry's stable
target. The official Rust template (`extension-template-rs`) uses the *identical*
`USE_UNSTABLE_C_API=1` + `TARGET_DUCKDB_VERSION=v1.5.5`, so our unstable‑C‑API
posture is normal for a duckdb‑rs extension, not an exception.

Caveat to watch: when DuckDB bumps its stable (v1.6+), community CI will try to
rebuild `mentat` against the new version. Because we pin the unstable C API to
v1.5.5, that rebuild will fail until we bump the `duckdb` crate + Makefile
`TARGET_DUCKDB_VERSION` together and PR a new `repo.ref`. That's the standard
Rust‑extension maintenance treadmill (`UPDATING.md` "Upgrading to a new DuckDB
version"), not a defect.

What our repo must expose for their CI (all already present in `crates/duckdb`):
- `Makefile` including
  `extension-ci-tools/makefiles/c_api_extensions/{base,rust}.Makefile`,
  with `EXTENSION_NAME=mentat`, `USE_UNSTABLE_C_API=1`,
  `TARGET_DUCKDB_VERSION=v1.5.5`, and the `configure`/`debug`/`release`/`test`
  targets. ✅ present.
- `EXTENSION_LIB_FILENAME`/`RUST_LIBNAME` override (our cdylib is
  `libmentat_duckdb.so`). ✅ present.
- The `extension-ci-tools` submodule. ✅ present (pinned to `main`;
  see gotcha below).
- Cross‑platform buildability: the extension is pure duckdb‑rs + rusqlite
  `bundled`, so it should build on linux/macOS/windows. `linux_amd64_musl` is a
  likely `excluded_platforms` (rusqlite/openssl-ish deps); mirror
  `quackformers`' exclusions to start, then relax.

**Gotcha (local, not registry):** our submodule is pinned to `main`
(`20bad04c`), while the community CI uses the tag `v1.5-variegata`. This only
affects *local* `make` builds, not the central build. If a local build breaks
after an upstream `main` change, pin the submodule to `v1.5-variegata` to match
what the registry uses.

### 1.4 Automation recipe (DuckDB)

First submission is **manual** (open the PR to `duckdb/community-extensions`,
their team reviews it). Automation is for the *subsequent* version bumps, and
even then the PR still needs a human merge upstream — you cannot fully
auto‑publish; upstream PR review is a hard gate.

Recommended: maintainer keeps a fork `gburd/community-extensions`. On a `v*`
tag, a job resolves the tag's SHA, edits `extensions/mentat/description.yml` in
the fork, and opens a PR to `duckdb/community-extensions` via
`peter-evans/create-pull-request`.

```yaml
  # New job in release.yml, gated the same way as the others (needs: gate).
  duckdb-registry-pr:
    name: PR to duckdb/community-extensions
    needs: gate
    runs-on: ubuntu-latest
    steps:
      - name: Resolve tag commit SHA
        id: sha
        run: echo "sha=${GITHUB_SHA}" >> "$GITHUB_OUTPUT"

      - name: Check out our fork of community-extensions
        uses: actions/checkout@v4
        with:
          repository: gburd/community-extensions      # maintainer's fork
          token: ${{ secrets.COMMUNITY_EXT_PAT }}     # PAT with repo scope on the fork
          path: community-extensions

      - name: Update descriptor
        working-directory: community-extensions
        run: |
          VER="${GITHUB_REF_NAME#v}"                  # 1.8.0
          SHA="${{ steps.sha.outputs.sha }}"
          mkdir -p extensions/mentat
          cat > extensions/mentat/description.yml <<EOF
          extension:
            name: mentat
            description: Datomic-compatible Datalog engine as a DuckDB extension
            version: ${VER}
            language: Rust
            build: cargo
            license: Apache-2.0
            maintainers:
              - gburd
            requires_toolchains: "rust;python3"
            excluded_platforms: "wasm_mvp;wasm_eh;wasm_threads;windows_amd64_mingw;linux_amd64_musl"
          repo:
            github: gburd/mentat
            ref: ${SHA}
          docs:
            hello_world: |
              LOAD mentat;
              SELECT * FROM mentat_query('/tmp/db.mentat', '[:find ?e :where [?e :db/ident ?a]]');
          EOF

      - name: Open PR upstream
        uses: peter-evans/create-pull-request@v6
        with:
          token: ${{ secrets.COMMUNITY_EXT_PAT }}
          path: community-extensions
          push-to-fork: gburd/community-extensions
          branch: mentat-${{ github.ref_name }}
          commit-message: "mentat ${{ github.ref_name }}"
          title: "Update mentat to ${{ github.ref_name }}"
          body: "Automated descriptor bump for mentat ${{ github.ref_name }} (SHA ${{ github.sha }})."
          # peter-evans opens the PR from the fork branch to the upstream base.
```

Secrets/tokens:
- `COMMUNITY_EXT_PAT` — a fine‑grained or classic PAT owned by the maintainer
  with **`repo`** (contents + pull‑requests) scope on the fork
  `gburd/community-extensions`, allowed to open PRs against the upstream. The
  default `GITHUB_TOKEN` cannot push to another repo, so a PAT (or a GitHub App
  token) is required.

Honest limits:
- **First submission is manual** (human PR + upstream review).
- **Every subsequent publish is a PR upstream** — CI can *open* it, but a DuckDB
  maintainer must **merge** it. There is no auto‑publish past that gate.
- `repo.github` must be the **GitHub** mirror (`gburd/mentat`), since their CI
  clones from GitHub — not the Codeberg canonical.

---

## 2. PGXN — `pg_mentat`

Upstream verified:
- Manager: `https://manager.pgxn.org/` (upload endpoint `POST /upload`).
- Tooling image source: `https://github.com/pgxn/docker-pgxn-tools`
  (Docker Hub image **`pgxn/pgxn-tools`**). This is the canonical current tool —
  ships `pgxn`, `pgxn-bundle`, `pgxn-release`, `pgrx-build-test`, etc.
- Bundle/release scripts read verbatim from that repo's `bin/pgxn-bundle` and
  `bin/pgxn-release`.

### 2.1 Is pg_mentat on PGXN? What publishing requires

Not currently on PGXN (no evidence of a prior release; `META.json` is present but
never bundled/uploaded). To publish you need:

1. A **PGXN account** at `https://manager.pgxn.org/`, approved by the PGXN admins
   (manual, can take a little while — do this first).
2. Then, per release: **`pgxn-bundle`** (validate META + `git archive` a zip) and
   **`pgxn-release`** (HTTP POST the zip to `manager.pgxn.org/upload` with basic
   auth). Both live in the **`pgxn/pgxn-tools`** container.

Confirmed commands & auth from `bin/pgxn-release`:
```
curl --user "$PGXN_USERNAME:$PGXN_PASSWORD" \
     -F 'submit=Release It!' -F "archive=@<dist>-<ver>.zip" \
     -H 'X-Requested-With: XMLHttpRequest' https://manager.pgxn.org/upload
```
Secrets: **`PGXN_USERNAME`** and **`PGXN_PASSWORD`** (your PGXN Manager creds).

`pgxnclient` (the old Python `pgxn` CLI) still exists but the canonical CI path is
the `pgxn/pgxn-tools` image; use it.

### 2.2 What the release ZIP must contain — and our two problems

`pgxn-bundle` does two things (verified from the script):
1. `pgxn validate-meta META.json` — **spec validation only** (structure per
   `meta-spec`), *not* a filesystem check that `provides.*.file` exists.
2. `git archive --prefix "<dist>-<ver>/" ... HEAD` — zips the **entire HEAD tree**
   with the dist prefix.

So the bundle step will *succeed* even with a broken `file` path. But the
resulting distribution is only correct if META matches the tree. Two real issues
in the current `META.json` (`~/ws/mentat/META.json`):

- **Wrong `provides.pg_mentat.file`.** It says `"pg_mentat/pg_mentat.control"`,
  but there is **no `pg_mentat/` dir at the repo root** — the control file lives
  at `crates/pg/pg_mentat/pg_mentat.control`. In the bundled zip the declared
  path won't resolve. Fix the `file` to the real path, e.g.
  `"crates/pg/pg_mentat/pg_mentat.control"` (and `docfile` — `README.md` at root
  is fine).
- **Stale `version`.** META is `1.7.0` (both top‑level and `provides.*.version`);
  releasing v1.8.0 requires bumping both. `Trunk.toml` is likewise `1.7.0`.
  `pgxn validate-meta` won't catch a version *mismatch with the tag* — you must
  keep them in sync yourself (or derive META version from the tag in CI).

Present & valid in META: `name`, `abstract`, `description`, `maintainer`,
`license` (`apache_2_0`), `meta-spec` (1.0.0 + url), `provides`, `resources`,
`tags`. Structurally `pgxn validate-meta` should pass once `version` is bumped;
the `file` path is a *correctness* bug, not a validation failure.

- **pgrx‑vs‑PGXN layout (the honest caveat).** Classic PGXN distributions are
  PGXS source trees: a consumer downloads the zip and runs `make && make install`
  driven by `provides.*.file` (a `.control`) + a PGXS `Makefile`. `pg_mentat` is a
  **pgrx** extension — no PGXS `Makefile`, and the `.control`/SQL are *generated*
  by `cargo pgrx package`, not shipped as static installable files at the paths
  PGXN expects. Consequences:
  - `pgxn-bundle`/`pgxn-release` **will upload** (the Manager only needs a valid
    META + a zip), so the release *appears* on PGXN and is mirrored/searchable.
  - But it is **not `make`‑installable from source** by a classic PGXN user, and
    PGXN's build‑farm won't build a pgrx tree. In practice PGXN becomes a
    *discovery/metadata* channel for `pg_mentat`, while real installation happens
    via the GitHub Release tarballs (already built by `release.yml`) or Trunk‑like
    binary registries. Decide whether a "listed but not source‑installable" PGXN
    entry is worth it. (Several pgrx extensions do exactly this — publish to PGXN
    for discoverability and ship binaries elsewhere.)

### 2.3 Trunk (`pgt.dev` / `Trunk.toml`) — skip it, the registry is defunct

`Trunk.toml` targets the Tembo "Trunk" PG binary registry (`pgt.dev`,
`trunk publish`). Verified 2025‑12:
- `pgt.dev` **no longer resolves in DNS** (dead host).
- The `tembo-io` GitHub org / `tembo-io/trunk` repo returns **Not Found** (gone).
- The `pg-trunk` crate on crates.io hasn't been updated since **April 2025**.

Tembo wound down the hosted Trunk registry. **Do not automate Trunk.** Leave
`Trunk.toml` in place as harmless metadata (or delete it); there's nothing live
to publish to. Revisit only if a successor registry appears.

### 2.4 Automation recipe (PGXN)

Fully automatable (no upstream human gate on publish — the account approval is a
one‑time human step). Add a job to `release.yml` gated the same way (`>= v1.7.0`,
so use the existing `gate` job). Fix META first (2.2) or the uploaded dist is
broken.

```yaml
  pgxn-release:
    name: Publish to PGXN
    needs: gate
    runs-on: ubuntu-latest
    container: pgxn/pgxn-tools      # canonical image; ships pgxn-bundle + pgxn-release
    steps:
      - name: Check out the repo
        uses: actions/checkout@v4
        with:
          submodules: false         # PGXN dist is pg_mentat only; keep the zip lean

      - name: Sync META.json version to the tag (belt-and-suspenders)
        run: |
          VER="${GITHUB_REF_NAME#v}"           # 1.8.0
          # Fail loudly if META wasn't bumped to match the tag.
          MJSON_VER="$(perl -MJSON=decode_json -E 'say decode_json(join "", <>)->{version}' META.json)"
          if [ "$MJSON_VER" != "$VER" ]; then
            echo "ERROR: META.json version ($MJSON_VER) != tag ($VER). Bump META.json." >&2
            exit 1
          fi

      - name: Bundle the release
        id: bundle
        run: pgxn-bundle                       # validates META, writes pg_mentat-<ver>.zip

      - name: Release on PGXN
        env:
          PGXN_USERNAME: ${{ secrets.PGXN_USERNAME }}
          PGXN_PASSWORD: ${{ secrets.PGXN_PASSWORD }}
        run: pgxn-release
```

Secrets: **`PGXN_USERNAME`**, **`PGXN_PASSWORD`** — add to the GitHub repo
settings once the PGXN account is approved.

One‑time maintainer steps:
1. Register at `https://manager.pgxn.org/` and get the account approved.
2. Add `PGXN_USERNAME` / `PGXN_PASSWORD` repo secrets.
3. **Fix `META.json`** (`provides.pg_mentat.file` → real path; bump `version` to
   the release version) — otherwise the uploaded distribution is malformed.
4. Accept the pgrx caveat (2.2): PGXN entry is discovery/metadata, not classic
   source‑install.

---

## 3. Summary table

| | DuckDB Community Extensions | PGXN |
|---|---|---|
| Publish mechanism | PR editing `extensions/mentat/description.yml` in `duckdb/community-extensions` | `pgxn-bundle` + `pgxn-release` (image `pgxn/pgxn-tools`) |
| Build/sign | **Central** (their CI, all platforms, DuckDB v1.5.5, `ci_tools_version v1.5-variegata`) | You bundle a zip; PGXN Manager just stores it |
| Our version‑lock | **Matches** (registry stable = v1.5.5 = our pin) ✅ | n/a |
| First time | Manual PR + upstream review | Create + get PGXN account approved |
| Per release (automatable?) | CI opens PR (needs `COMMUNITY_EXT_PAT`); **merge is a human gate** | Fully automatable (`PGXN_USERNAME`/`PGXN_PASSWORD`) |
| Secrets | `COMMUNITY_EXT_PAT` (fork+PR scope) | `PGXN_USERNAME`, `PGXN_PASSWORD` |
| Non‑standard for us | Unstable‑C‑API version‑lock (normal for duckdb‑rs; breaks on DuckDB bumps) | `META.json` `file` path wrong + version stale; pgrx tree not classic `make`‑installable |
| `repo.github` / source | Must be **GitHub mirror** `gburd/mentat` (their CI clones GitHub) | Bundles whatever is at HEAD of the checked‑out repo |
| Trunk (`pgt.dev`) | — | **Defunct — do not automate** (`pgt.dev` dead, `tembo-io/trunk` gone) |

## 4. Verified sources

- DuckDB community: `github.com/duckdb/community-extensions`
  (`README.md`, `UPDATING.md`, `scripts/build.py`,
  `.github/workflows/build.yml` → `DUCKDB_LATEST_STABLE: v1.5.5`,
  `_extension_distribution.yml@v1.5-variegata`).
- Real descriptors: `extensions/quackformers/description.yml` (pure Rust,
  `build: cargo`), `extensions/crypto/description.yml`,
  `extensions/waddle/description.yml` (in `UPDATING.md`).
- Rust template: `github.com/duckdb/extension-template-rs`
  (`Makefile` → `USE_UNSTABLE_C_API=1`, `TARGET_DUCKDB_VERSION=v1.5.5`;
  `.github/workflows/MainDistributionPipeline.yml`).
- PGXN tooling: `github.com/pgxn/docker-pgxn-tools`
  (`README.md`, `bin/pgxn-bundle`, `bin/pgxn-release`); image `pgxn/pgxn-tools`;
  upload endpoint `https://manager.pgxn.org/upload`.
- Local: `~/ws/mentat/META.json`, `~/ws/mentat/Trunk.toml`,
  `~/ws/mentat/crates/duckdb/Makefile`, `~/ws/mentat/crates/duckdb/Cargo.toml`,
  `~/ws/mentat/.github/workflows/release.yml`,
  `~/ws/mentat/crates/pg/pg_mentat/pg_mentat.control`.
- Trunk defunct: `pgt.dev` no DNS; `github.com/tembo-io/trunk` 404;
  crates.io `pg-trunk` last updated 2025‑04.
