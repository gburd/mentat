#!/usr/bin/env bash
# ci/announce.sh VERSION
#
# Draft a PostgreSQL-style release announcement for pg_mentat and submit it to
# the postgresql.org news/announce system (a login-authorized web form;
# approved news feeds the pgsql-announce list). Builds the announcement text
# from CHANGELOG.md + a fixed footer, then posts it using PGORG_* secrets. If
# those aren't fully set it prints the drafted announcement and exits 0, so a
# release never fails just because announcing isn't configured.
#
# NOTE (honest): the postgresql.org news system can only be *submitted* to via
# an authenticated two-step form and lands in a two-moderator queue -- it is
# NEVER auto-published. There is no way to auto-post to pgsql-announce; that
# gate is postgresql.org's. Even fully configured, this queues an article for
# human moderation. In the common case (secrets unset) it is a maintainer
# draft to paste into https://www.postgresql.org/account/news/new/ .
#
# Only feature (X.Y.0) and major (X.0.0) releases are announced. A patch
# release (X.Y.Z, Z>0) is skipped unless ANNOUNCE=1 or its CHANGELOG section
# has a "### Security" heading.
set -euo pipefail

# The extension's version lives in crates/pg/pg_mentat/pg_mentat.control
# (release.yml's gate checks it equals the workspace Cargo.toml, pg_mentat's
# Cargo.toml and META.json); it is the default when no VERSION is given.
VERSION="${1:-$(sed -n "s/^default_version = '\\(.*\\)'/\\1/p" crates/pg/pg_mentat/pg_mentat.control)}"
VERSION="${VERSION#v}"
REPO_WEB="https://codeberg.org/gregburd/mentat"
# Supported PostgreSQL majors, from the pgN features in pg_mentat's Cargo.toml.
PG_RANGE="$(sed -nE 's/^pg([0-9]+) = .*/\1/p' crates/pg/pg_mentat/Cargo.toml | sort -n | sed -n '1p;$p' | paste -sd' ' | sed 's/ / through /')"
[ -n "$PG_RANGE" ] || PG_RANGE="13 and later"
CHANGELOG="CHANGELOG.md"

# --- The CHANGELOG section for VERSION ----------------------------------------
# pg_mentat has no CHANGELOG of its own: the root CHANGELOG.md covers both the
# embedded store and the pg_mentat extension, which share one version.
# Headings look like "## [1.15.0] — summary". Print from the
# heading whose version is exactly VERSION up to the next "## " heading.
changelog_section() {
  [ -f "$CHANGELOG" ] || return 0
  awk -v ver="$VERSION" '
    /^## / {
      if (inblk) exit
      h = $0; sub(/^## +\[?v?/, "", h); sub(/[^0-9A-Za-z.+-].*$/, "", h)
      if (h == ver) inblk = 1
    }
    inblk { print }
  ' "$CHANGELOG"
}
CHANGES="$(changelog_section)"

# --- Gate: announce feature/major releases, not trivial patch bumps -----------
patch="$(printf '%s\n' "$VERSION" | sed -nE 's/^[0-9]+\.[0-9]+\.([0-9]+).*/\1/p')"
if [ -n "$patch" ] && [ "$patch" -gt 0 ] && [ "${ANNOUNCE:-}" != 1 ] \
   && ! printf '%s\n' "$CHANGES" | grep -q '^### Security'; then
  echo "pg_mentat ${VERSION} is a patch release without a '### Security' section; not announcing (ANNOUNCE=1 to force)."
  exit 0
fi

[ -n "$CHANGES" ] || CHANGES="See ${REPO_WEB}/src/branch/master/CHANGELOG.md"

TITLE="pg_mentat ${VERSION} released"
BODY="$(cat <<EOF
pg_mentat ${VERSION} is now available.

pg_mentat is a PostgreSQL extension, built with pgrx, that provides a
Datomic-compatible Datalog query engine with a native EDN data type: Datalog
queries with rules, aggregates and OR/NOT clauses, EDN transactions with tempid
resolution and upserts, the Pull API, and time-travel (as-of, since, history)
queries, all as SQL functions. It supports PostgreSQL ${PG_RANGE}.

Changes in ${VERSION} (the changelog is shared with the Mentat embedded
store, so entries for its other backends are included):

${CHANGES}

Install from PGXN (\`pgxn install pg_mentat\`; a source distribution, built
with cargo pgrx) or from source:
  ${REPO_WEB}

Documentation is in the repository.
EOF
)"

# --- Submit (or dry-run) -----------------------------------------------------
# The postgresql.org news system (pgweb) requires: a password-login account
# (no OAuth/2FA), an APPROVED Organisation you manage, a confirmed org email,
# and one or more NewsTag ids. Two-step form: create the article
# (POST /account/news/new/) then confirm-submit (POST /account/news/<id>/submit/),
# after which two moderators must approve. CI cannot bootstrap the org/email/tags,
# so unless ALL of these are provided we print the drafted announcement and
# exit 0.
#   PGORG_USER, PGORG_PASSWORD  - a password-login postgresql.org account
#   PGORG_ORG_ID                - approved Organisation id (managed by that account)
#   PGORG_EMAIL_ID              - confirmed OrganisationEmail id (same org)
#   PGORG_TAGS                  - space-separated NewsTag ids (see
#                                 https://www.postgresql.org/about/news/taglist.json/)
BASE="https://www.postgresql.org"
if [ -z "${PGORG_USER:-}" ] || [ -z "${PGORG_PASSWORD:-}" ] \
   || [ -z "${PGORG_ORG_ID:-}" ] || [ -z "${PGORG_EMAIL_ID:-}" ] || [ -z "${PGORG_TAGS:-}" ]; then
  echo "PGORG_* not fully set (need USER, PASSWORD, ORG_ID, EMAIL_ID, TAGS) --"
  echo "printing the drafted announcement; submit it by hand at ${BASE}/account/news/new/ :"
  echo "----------------------------------------------------------------------"
  echo "Title: ${TITLE}"
  echo
  echo "${BODY}"
  echo "----------------------------------------------------------------------"
  exit 0
fi

CJ="$(mktemp)"; trap 'rm -f "$CJ"' EXIT
csrf() { grep -oE 'name="csrfmiddlewaretoken" value="[^"]+"' | head -1 | sed 's/.*value="//;s/"//'; }

# 1. Log in (Django LoginView).
LOGIN_CSRF="$(curl -sS -c "$CJ" "$BASE/account/login/" | csrf)"
curl -sS -c "$CJ" -b "$CJ" -e "$BASE/account/login/" -H "Origin: $BASE" \
  --data-urlencode "csrfmiddlewaretoken=$LOGIN_CSRF" \
  --data-urlencode "username=$PGORG_USER" \
  --data-urlencode "password=$PGORG_PASSWORD" \
  --data-urlencode "next=/account/news/new/" \
  "$BASE/account/login/" >/dev/null

# 2. Create the article (state CREATED). org/email/tags are required.
NEW_CSRF="$(curl -sS -c "$CJ" -b "$CJ" "$BASE/account/news/new/" | csrf)"
tagargs=(); for t in $PGORG_TAGS; do tagargs+=(--data-urlencode "tags=$t"); done
curl -sS -c "$CJ" -b "$CJ" -e "$BASE/account/news/new/" -H "Origin: $BASE" \
  --data-urlencode "csrfmiddlewaretoken=$NEW_CSRF" \
  --data-urlencode "org=$PGORG_ORG_ID" \
  --data-urlencode "email=$PGORG_EMAIL_ID" \
  --data-urlencode "title=$TITLE" \
  --data-urlencode "content=$BODY" \
  --data-urlencode "date=$(date +%F)" \
  "${tagargs[@]}" \
  "$BASE/account/news/new/" >/dev/null

# 3. Find the new article id, then confirm-submit it into the moderation queue.
ID="$(curl -sS -c "$CJ" -b "$CJ" "$BASE/account/edit/news/" \
      | grep -oE '/account/news/[0-9]+/' | grep -oE '[0-9]+' | sort -rn | head -1)"
if [ -z "$ID" ]; then
  echo "::warning::could not find the new article id; finish manually at $BASE/account/edit/news/"
  exit 0
fi
SUB_CSRF="$(curl -sS -c "$CJ" -b "$CJ" "$BASE/account/news/$ID/submit/" | csrf)"
code="$(curl -sS -o /dev/null -w '%{http_code}' -c "$CJ" -b "$CJ" \
  -e "$BASE/account/news/$ID/submit/" -H "Origin: $BASE" \
  --data-urlencode "csrfmiddlewaretoken=$SUB_CSRF" \
  --data-urlencode "confirm=on" \
  "$BASE/account/news/$ID/submit/")"
echo "news submit HTTP ${code} (article ${ID}) -- now pending moderation (two moderators approve)."
case "$code" in
  2*|3*) echo "Announcement queued." ;;
  *) echo "::warning::submit returned HTTP ${code}; finish manually at $BASE/account/edit/news/" ;;
esac
