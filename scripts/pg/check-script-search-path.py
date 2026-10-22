#!/usr/bin/env python3
"""Fail if pg_mentat's install SQL calls a function through the search path
while the script runs.

pg_mentat is a trusted extension: CREATE EXTENSION and ALTER EXTENSION UPDATE
run their scripts as the bootstrap superuser, with the extension schema
(public) on the search path. A call such as format(...) made while the script
runs resolves through that path, so a function of the same name a database
owner created in public would run as superuser (fixed in 1.17.2). Code that
runs at script time is the body of a DO block, and top-level SELECT / PERFORM
statements; CREATE FUNCTION bodies run later, as the caller, and are not
checked here.

Each such call must be schema-qualified (pg_catalog.format, mentat.x). DO
blocks also pin the path (pg_catalog.set_config('search_path', ..., true),
restored before the block ends) as a second layer for operators, which can't
be qualified this way; this check doesn't rely on that. SET [LOCAL]
search_path is refused: it lasts until the end of the script's transaction, so
every later unqualified CREATE in the script would land in pg_catalog. Plain
SQL keywords and constructs that look like calls (VALUES (, IN (, CHECK (,
ARRAY[...], CASE, ...) are not function calls and are ignored.

Usage: scripts/pg/check-script-search-path.py [files...]
(default: the fresh-install SQL in crates/pg/pg_mentat/sql/[0-9]*.sql and the
upgrade scripts from 1.17.1 on).
"""
import glob
import re
import sys

NOT_CALLS = {
    "if", "in", "select", "values", "exists", "array", "case", "not", "and",
    "or", "row", "any", "all", "some", "cast", "where", "check", "set", "using",
    "table", "returns", "on", "conflict", "enum", "from", "as", "with", "over",
    "filter", "within", "default", "references", "primary", "unique", "key",
    "foreign", "index", "include", "partition", "by", "range", "list", "for",
    "loop", "while", "return", "raise", "perform", "execute", "into", "when",
    "then", "else", "elsif", "end", "begin", "declare", "exception", "is",
    "null", "true", "false", "like", "ilike", "between", "type", "only",
    "lateral", "join", "union", "intersect", "except", "limit", "offset",
    "order", "group", "having", "distinct", "returning", "update", "insert",
    "delete", "do", "language", "plpgsql", "sql", "trigger", "function",
    "procedure", "view", "schema", "sequence", "extension", "grant", "revoke",
    "to", "public", "constraint", "deferrable", "initially", "deferred",
    "immediate", "nulls", "first", "last", "asc", "desc", "collate",
    "timestamp", "timestamptz", "time", "interval", "numeric", "decimal",
    "varchar", "char", "character", "bit", "varbit", "float", "double",
    "precision",
    # SQL constructs that look like calls but aren't resolved through the
    # search path (the parser handles them):
    "coalesce", "nullif", "greatest", "least", "extract", "position",
    "substring", "overlay", "trim", "treat", "xmlelement", "grouping",
}
SET_LOCAL = re.compile(r"\bSET\s+(LOCAL\s+)?search_path\b", re.I)
# A call: an identifier not preceded by '.', '"' or a word char, followed by '('.
CALL = re.compile(r"(?<![.\w\"])([A-Za-z_][A-Za-z0-9_]*)\s*\(")


def strip_comments_and_strings(sql):
    sql = re.sub(r"--[^\n]*", "", sql)
    sql = re.sub(r"/\*[\s\S]*?\*/", "", sql)
    # single-quoted literals (format strings etc.): calls inside them are text
    sql = re.sub(r"'(?:[^']|'')*'", "''", sql)
    return sql


def function_bodies_removed(sql):
    # CREATE [OR REPLACE] FUNCTION/PROCEDURE ... AS $tag$ ... $tag$
    return re.sub(
        r"CREATE\s+(?:OR\s+REPLACE\s+)?(?:FUNCTION|PROCEDURE)[\s\S]*?\bAS\s+(\$[A-Za-z_]*\$)[\s\S]*?\1",
        " ", sql, flags=re.I)


def check(path):
    raw = open(path).read()
    sql = function_bodies_removed(strip_comments_and_strings(raw))
    problems = []
    for m in re.finditer(r"\bDO\s+(\$[A-Za-z_]*\$)([\s\S]*?)\1", sql, re.I):
        body = m.group(2)
        if SET_LOCAL.search(body):
            problems.append(("DO block", "SET [LOCAL] search_path (leaks past the block)"))
        for c in CALL.finditer(body):
            if c.group(1).lower() not in NOT_CALLS:
                problems.append(("DO block", c.group(1)))
    outside = re.sub(r"\bDO\s+(\$[A-Za-z_]*\$)[\s\S]*?\1", " ", sql, flags=re.I)
    for stmt in outside.split(";"):
        s = stmt.strip()
        if re.match(r"(SELECT|PERFORM)\b", s, re.I):
            for c in CALL.finditer(s):
                if c.group(1).lower() not in NOT_CALLS:
                    problems.append(("top-level " + s.split()[0].upper(), c.group(1)))
    return problems


def main(argv):
    root = "crates/pg/pg_mentat/sql"
    files = argv[1:] or sorted(glob.glob(f"{root}/[0-9]*.sql")) + sorted(
        p for p in glob.glob(f"{root}/pg_mentat--*--*.sql")
        if tuple(int(x) for x in re.findall(r"--(\d+)\.(\d+)\.(\d+)\.sql$", p)[0]) >= (1, 17, 2))
    bad = 0
    for f in files:
        for where, name in check(f):
            if "(" in name:
                print(f"{f}: {where}: {name}")
            else:
                print(f"{f}: {where} calls {name}() through the search path; "
                      f"write pg_catalog.{name}()")
            bad += 1
    if bad:
        sys.exit(1)
    print(f"ok: {len(files)} files, no script-time call resolved through the search path")


if __name__ == "__main__":
    main(sys.argv)
