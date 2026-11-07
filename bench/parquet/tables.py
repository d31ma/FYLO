#!/usr/bin/env python3
"""Print the report's Markdown tables from one or more results.jsonl files."""
import json
import sys

ENGINES = ["fylo-files", "parquet"]
TIMED = [
    ("batch_insert_1000", "Batch insert, 1,000 docs (one call)"),
    ("put_single", "Insert one doc (durable)"),
    ("patch_single", "Patch one doc (durable)"),
    ("delete_single", "Delete one doc (durable)"),
    ("get_by_id", "Point read by id"),
    ("get_latest", "getLatest"),
    ("find_eq_indexed", "Equality query, bucket = k (0.1%)"),
    ("find_range", "Range query, k <= score < k+1 (1%)"),
    ("filter_project_3", "Filter age >= 90 (10%) + project 3 fields"),
    ("full_scan_project_3", "Full scan + project 3 fields"),
    ("open", "Open (fresh process state)"),
    ("first_get_after_open", "First read after open"),
    ("compact_after_load", "Compaction after bulk load"),
    ("compact_after_writes", "Compaction after the single-doc writes"),
]


def load(paths):
    rows = []
    for path in paths:
        with open(path) as handle:
            rows.extend(json.loads(line) for line in handle if line.strip())
    return rows


def duration(us):
    if us >= 1e6:
        return f"{us / 1e6:.2f} s"
    if us >= 1e3:
        return f"{us / 1e3:.2f} ms"
    return f"{us:.0f} us"


def cell(row):
    stats = row["stats"]
    if stats.get("count", 0) == 1:
        return duration(stats["p50_us"])
    return f"{duration(stats['p50_us'])} / {duration(stats['p95_us'])}"


def cases(rows):
    seen = []
    for row in rows:
        key = (row["location"], row["shape"], row.get("n"))
        if row.get("phase") == "run" and key not in seen:
            seen.append(key)
    return seen


def find(rows, location, shape, n, engine, workload):
    for row in rows:
        if (row.get("phase"), row["location"], row["shape"], row.get("n"), row["engine"], row["workload"]) == (
            "run", location, shape, n, engine, workload):
            return row
    return None


def timed_tables(rows):
    for location, shape, n in cases(rows):
        print(f"\n#### {shape}, {n:,} documents, {location}\n")
        print("| Workload | FYLO files p50 / p95 | Parquet p50 / p95 | Parquet speed-up at p50 |")
        print("| --- | ---: | ---: | ---: |")
        for workload, label in TIMED:
            fylo = find(rows, location, shape, n, "fylo-files", workload)
            parquet = find(rows, location, shape, n, "parquet", workload)
            if not fylo and not parquet:
                continue
            ratio = ""
            if fylo and parquet and parquet["stats"]["p50_us"] > 0 and fylo["stats"].get("count"):
                factor = fylo["stats"]["p50_us"] / parquet["stats"]["p50_us"]
                ratio = f"{factor:.1f}x" if factor >= 1 else f"{1 / factor:.1f}x slower"
            print(f"| {label} | {cell(fylo) if fylo else '-'} | {cell(parquet) if parquet else '-'} | {ratio} |")
        for engine in ENGINES:
            load_row = find(rows, location, shape, n, engine, "bulk_load")
            if load_row:
                extra = load_row["extra"]
                print(f"\n{engine} bulk load: {extra['documents']:,} docs in {extra['seconds']:.1f} s "
                      f"({extra['docs_per_s']:,.0f} docs/s).", end="")
        print()


def size_tables(rows):
    print("\n#### Storage footprint\n")
    print("| Case | Engine | When | Files | Logical bytes | Allocated bytes | Bytes/doc (allocated) |")
    print("| --- | --- | --- | ---: | ---: | ---: | ---: |")
    for location, shape, n in cases(rows):
        for engine in ENGINES:
            for workload in ("footprint_after_load", "footprint_final"):
                row = find(rows, location, shape, n, engine, workload)
                if not row:
                    continue
                f = row["extra"]["footprint"]
                print(f"| {shape} {n:,} {location} | {engine} | {workload.replace('footprint_', '')} | "
                      f"{f['files']:,} | {f['logical_bytes']:,} | {f['allocated_bytes']:,} | "
                      f"{f['allocated_bytes'] / n:,.0f} |")
    print("\n#### Parquet size by codec (live data, one sorted file)\n")
    print("| Case | none | snappy | zstd(3) |")
    print("| --- | ---: | ---: | ---: |")
    for location, shape, n in cases(rows):
        sizes = {}
        for row in rows:
            if (row.get("phase"), row["location"], row["shape"], row.get("n"), row["workload"]) == (
                    "run", location, shape, n, "size_by_codec"):
                sizes[row["extra"]["codec"]] = row["extra"]["footprint"]["logical_bytes"]
        if sizes:
            print(f"| {shape} {n:,} {location} | " + " | ".join(
                f"{sizes.get(codec, 0):,}" for codec in ("none", "snappy", "zstd(3)")) + " |")


def other(rows):
    print("\n#### Crash, damage and rebuild\n")
    for row in rows:
        if row.get("phase") in ("crash", "damage") or row.get("phase") == "load":
            print(json.dumps({k: row[k] for k in row if k not in ("stats",)}))


if __name__ == "__main__":
    data = load(sys.argv[1:])
    timed_tables(data)
    size_tables(data)
    other(data)
