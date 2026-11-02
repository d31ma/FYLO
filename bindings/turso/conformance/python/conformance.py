# fylo link conformance for pyturso: the stock package, with fylo link's
# import hook in its environment, opens a FYLO root as its database.
# Usage: python conformance.py <fylo-root> <plain-db-path>
import json
import os
import sys

import turso

results = []


def check(name, body):
    try:
        body()
        results.append("PASS " + name)
    except Exception as error:  # noqa: BLE001 - every failure is reported
        results.append(f"FAIL {name}: {error!r}")


def fails_with(code, body):
    try:
        body()
    except turso.Error as error:
        if code not in str(error):
            raise AssertionError(f"expected {code}, got {error}") from error
        return
    raise AssertionError(f"expected {code}, got success")


def ids(db, query, *args):
    return [row[0] for row in db.execute(query, args).fetchall()]


def expect(condition, message):
    if not condition:
        raise AssertionError(message)


root, plain = sys.argv[1], sys.argv[2]
db = turso.connect(root, isolation_level=None)


def collections_are_tables():
    count = db.execute("SELECT count(*) FROM products").fetchone()[0]
    expect(count == 60, f"count {count}")
    db.execute("SELECT name FROM products ORDER BY name LIMIT 1").fetchone()


def pushdown_matches_a_full_scan():
    cases = [("price = ?", "price + 0 = ?", 7), ("price > ?", "price + 0 > ?", 15), ("name = ?", "name || '' = ?", "item-0042")]
    for pushed, scanned, value in cases:
        a = ids(db, f"SELECT id FROM products WHERE {pushed} ORDER BY id", value)
        b = ids(db, f"SELECT id FROM products WHERE {scanned} ORDER BY id", value)
        expect(a == b and a, f"{pushed}: {a} vs {b}")


def writes_commit_through_fylo():
    cursor = db.execute("INSERT INTO notes (doc) VALUES (?)", ('{"text":"hello","n":1}',))
    expect(cursor.rowcount == 1, f"insert changed {cursor.rowcount} rows")
    inserted, doc = db.execute("SELECT id, doc FROM notes").fetchone()
    expect(json.loads(doc)["text"] == "hello", f"doc {doc}")
    db.execute("UPDATE notes SET doc = json_set(doc, '$.n', 2) WHERE id = ?", (inserted,))
    n = db.execute("SELECT json_extract(doc, '$.n') FROM notes WHERE id = ?", (inserted,)).fetchone()[0]
    expect(n == 2, f"n {n}")
    cursor = db.execute("DELETE FROM products WHERE name = ?", ("item-0002",))
    expect(cursor.rowcount == 1, f"delete changed {cursor.rowcount} rows")


def transactions_are_whole():
    db.execute("BEGIN")
    db.execute("INSERT INTO notes (doc) VALUES (?), (?)", ('{"t":1}', '{"t":2}'))
    db.execute("COMMIT")
    db.execute("BEGIN")
    db.execute("DELETE FROM notes")
    db.execute("ROLLBACK")
    count = db.execute("SELECT count(*) FROM notes").fetchone()[0]
    expect(count == 3, f"notes {count}")


def a_second_collection_is_out_of_scope():
    db.execute("BEGIN")
    db.execute("INSERT INTO notes (doc) VALUES (?)", ('{"scope":1}',))
    fails_with("EFYLO_TXN_SCOPE", lambda: db.execute("""INSERT INTO products (doc) VALUES ('{"name":"x"}')"""))
    try:
        db.execute("ROLLBACK")
    except turso.Error:
        pass  # the failed statement already ended the transaction
    count = db.execute("SELECT count(*) FROM notes").fetchone()[0]
    expect(count == 3, f"the transaction's write survived: notes {count}")


def fylo_errors_keep_their_codes():
    fails_with("EARRAYOFOBJECTS", lambda: db.execute("INSERT INTO notes (doc) VALUES (?)", ('{"tags":[{"a":1}]}',)))


def the_queue_is_tables():
    db.execute("INSERT INTO fylo_queue_publish (topic, payload) VALUES ('jobs', ?)", ('{"job":1}',))
    claimed = db.execute(
        "SELECT id, receipt, attempt, payload FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'"
    ).fetchone()
    db.execute(
        "INSERT INTO fylo_queue_ack (topic, group_name, id, receipt) VALUES ('jobs', 'workers', ?, ?)",
        (claimed[0], claimed[1]),
    )
    version = db.execute("SELECT value FROM fylo_info WHERE name = 'turso_core'").fetchone()[0]
    expect(version == "0.8.3", f"version {version}")


def a_plain_path_opens_as_stock_turso():
    file = turso.connect(plain, isolation_level=None)
    file.execute("CREATE TABLE t (x INTEGER)")
    file.execute("INSERT INTO t VALUES (1), (2)")
    total = file.execute("SELECT sum(x) FROM t").fetchone()[0]
    file.close()
    expect(total == 3, f"total {total}")
    expect(os.path.isfile(plain), "no database file")


check("collections are tables", collections_are_tables)
check("pushdown returns what a full scan returns", pushdown_matches_a_full_scan)
check("insert, update and delete commit through FYLO", writes_commit_through_fylo)
check("a transaction commits whole and rolls back whole", transactions_are_whole)
check("a second collection in one transaction is EFYLO_TXN_SCOPE", a_second_collection_is_out_of_scope)
check("FYLO errors keep their codes", fylo_errors_keep_their_codes)
check("the queue is tables", the_queue_is_tables)
db.close()
check("a plain database path opens as stock Turso", a_plain_path_opens_as_stock_turso)

print("\n".join(results))
sys.exit(1 if any(line.startswith("FAIL") for line in results) else 0)
