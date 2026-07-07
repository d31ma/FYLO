"""Fylo client — drives the `fylo` binary's persistent NDJSON loop.

No pip dependencies. Requires the `fylo` binary on PATH (brew/scoop) or an
explicit path. One long-lived subprocess keeps the engine warm across calls.

    from fylo import Fylo

    with Fylo("/path/to/db") as db:
        db.create_collection("users")
        doc_id = db.put_data("users", {"name": "Ada", "role": "admin"})
        doc = db.get_latest("users", doc_id)
        admins = db.find_docs("users", {"$ops": [{"role": {"$eq": "admin"}}]})

Each operation method builds the request, sends it, and returns the operation's
`result` (raising FyloError on failure). Method names follow Python's snake_case
convention. `request(op)` remains as a raw escape hatch returning the full
response dict — use it for ops without a dedicated method (branching, schema).
"""

import json
import subprocess
import threading


class FyloError(RuntimeError):
    pass


class Fylo:
    def __init__(self, root, binary="fylo", worm=False):
        args = [binary, "exec", "--loop", "--root", root]
        if worm:
            args.append("--worm")
        self._proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",  # protocol is UTF-8; don't fall back to the Windows code page
            bufsize=1,
        )
        self._lock = threading.Lock()

    def request(self, op):
        """Send one raw machine-protocol op; return the full response dict."""
        line = json.dumps(op)
        with self._lock:  # ponytail: one call in flight; drop the lock only if you pipeline
            if self._proc.poll() is not None:
                raise FyloError("fylo process has exited")
            self._proc.stdin.write(line + "\n")
            self._proc.stdin.flush()
            reply = self._proc.stdout.readline()
        if not reply:
            raise FyloError("fylo closed the stream (stderr may have details)")
        return json.loads(reply)

    def _op(self, op, **fields):
        payload = {"op": op}
        for key, value in fields.items():
            if value is not None:
                payload[key] = value
        response = self.request(payload)
        if not response.get("ok"):
            raise FyloError((response.get("error") or {}).get("message", "fylo error"))
        return response.get("result")

    # --- Collections ---
    def create_collection(self, collection, kind="document"):
        return self._op("createCollection", collection=collection, kind=kind)

    def drop_collection(self, collection):
        return self._op("dropCollection", collection=collection)

    def inspect_collection(self, collection):
        return self._op("inspectCollection", collection=collection)

    def rebuild_collection(self, collection):
        return self._op("rebuildCollection", collection=collection)

    # --- Documents ---
    def put_data(self, collection, data):
        return self._op("putData", collection=collection, data=data)

    def batch_put_data(self, collection, batch):
        return self._op("batchPutData", collection=collection, batch=batch)

    def get_doc(self, collection, id):
        return self._op("getDoc", collection=collection, id=id)

    def get_latest(self, collection, id, only_id=False):
        return self._op("getLatest", collection=collection, id=id, onlyId=only_id)

    def patch_doc(self, collection, id, new_doc, old_doc=None):
        return self._op("patchDoc", collection=collection, id=id, newDoc=new_doc, oldDoc=old_doc)

    def patch_docs(self, collection, update):
        return self._op("patchDocs", collection=collection, update=update)

    def del_doc(self, collection, id):
        return self._op("delDoc", collection=collection, id=id)

    def del_docs(self, collection, criteria):
        return self._op("delDocs", collection=collection, delete=criteria)

    def restore_doc(self, collection, id):
        return self._op("restoreDoc", collection=collection, id=id)

    # --- Query ---
    def find_docs(self, collection, query):
        return self._op("findDocs", collection=collection, query=query)

    def find_deleted_docs(self, collection, query=None):
        return self._op("findDeletedDocs", collection=collection, query=query or {})

    def join_docs(self, join):
        return self._op("joinDocs", join=join)

    def execute_sql(self, sql):
        return self._op("executeSQL", sql=sql)

    def sql(self, query):
        """Run raw SQL, built with a native f-string: db.sql(f"... {x}").
        Values are inlined verbatim — escape/validate untrusted input yourself.
        """
        return self.execute_sql(query)

    def import_bulk_data(self, collection, url, limit_or_options=None):
        return self._op(
            "importBulkData", collection=collection, url=url, limitOrOptions=limit_or_options
        )

    # Collection-scoped facade with short method names, so
    # `db.collection("users").put(data)` reads like the browser client.
    def collection(self, name):
        return _Collection(self, name)

    def close(self):
        if self._proc.poll() is None:
            self._proc.stdin.close()
            self._proc.wait(timeout=30)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def __getattr__(self, name):
        # Sugar: `db.users.put(...)` -> `db.collection("users").put(...)`. Only
        # fires for names that aren't real attributes; skip private/dunder.
        if name.startswith("_"):
            raise AttributeError(name)
        return _Collection(self, name)


class _Collection:
    """A collection-scoped view; methods drop the leading collection argument."""

    def __init__(self, db, name):
        self._db = db
        self._name = name

    def create(self, kind="document"):
        return self._db.create_collection(self._name, kind)

    def drop(self):
        return self._db.drop_collection(self._name)

    def inspect(self):
        return self._db.inspect_collection(self._name)

    def rebuild(self):
        return self._db.rebuild_collection(self._name)

    def put(self, data):
        return self._db.put_data(self._name, data)

    def get(self, id):
        return self._db.get_doc(self._name, id)

    def latest(self, id, only_id=False):
        return self._db.get_latest(self._name, id, only_id)

    def patch(self, id, new_doc, old_doc=None):
        return self._db.patch_doc(self._name, id, new_doc, old_doc)

    def delete(self, id):
        return self._db.del_doc(self._name, id)

    def restore(self, id):
        return self._db.restore_doc(self._name, id)

    def find(self, query):
        return self._db.find_docs(self._name, query)
