"""Two-track bench: wall + peak RSS, Rust vs pure, per workload.

Workloads (see data/rs-port-sqlparse/WORKLOAD.md in rustsmith): the frozen
121-statement mixed DDL/DML script at 1x (~13 KB) and 8x (~106 KB), plus a
single-statement micro shape. Every figure is labeled by workload; both
sides run in the same installed wheel (`SQLPARSE_PURE=1` selects pure).

Usage:
  python benches/bench_two_track.py            # wall table (in-process)
  python benches/bench_two_track.py --rss      # peak-RSS table (subprocesses)
  python benches/bench_two_track.py --work split-1x --side rust --iters N
"""

import os
import resource
import subprocess
import sys
import time

STATEMENTS = (
    [
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER "
        "NOT NULL, total NUMERIC(10,2), placed_at TIMESTAMP DEFAULT now(), "
        "status VARCHAR(20));"
    ]
    + [
        f"INSERT INTO orders (id, customer_id, total, status) VALUES "
        f"({i}, {i % 17}, {i * 1.5:.2f}, 'shipped');"
        for i in range(60)
    ]
    + [
        "SELECT o.id, c.name, o.total FROM orders o "
        "JOIN customers c ON c.id = o.customer_id "
        f"WHERE o.status = 'shipped' AND o.total > {i} "
        "ORDER BY o.placed_at DESC LIMIT 50;"
        for i in range(40)
    ]
    + [
        f"UPDATE orders SET status = 'refunded' WHERE id = {i}; "
        f"-- refund batch {i}\n"
        for i in range(20)
    ]
)

SCRIPT_1X = "\n".join(STATEMENTS)
SCRIPT_8X = "\n".join(STATEMENTS * 8)
MICRO = "select a, b from t where x > 1;"


def work_split_1x(sqlparse):
    return sqlparse.split(SCRIPT_1X)


def work_split_8x(sqlparse):
    return sqlparse.split(SCRIPT_8X)


def work_parse_1x(sqlparse):
    return sqlparse.parse(SCRIPT_1X)


def work_parse_8x(sqlparse):
    return sqlparse.parse(SCRIPT_8X)


def work_format_reindent_1x(sqlparse):
    return sqlparse.format(SCRIPT_1X, reindent=True, keyword_case="upper")


def work_format_reindent_8x(sqlparse):
    return sqlparse.format(SCRIPT_8X, reindent=True, keyword_case="upper")


def work_format_light_1x(sqlparse):
    return sqlparse.format(SCRIPT_1X, keyword_case="upper", strip_comments=True)


def work_format_light_8x(sqlparse):
    return sqlparse.format(SCRIPT_8X, keyword_case="upper", strip_comments=True)


def work_parse_single(sqlparse):
    return sqlparse.parse(MICRO)


def work_format_single(sqlparse):
    return sqlparse.format(MICRO, reindent=True, keyword_case="upper")


WORKLOADS = {
    "split-1x": (work_split_1x, 40),
    "split-8x": (work_split_8x, 6),
    "parse-1x": (work_parse_1x, 25),
    "parse-8x": (work_parse_8x, 4),
    "format-reindent-1x": (work_format_reindent_1x, 20),
    "format-reindent-8x": (work_format_reindent_8x, 3),
    "format-light-1x": (work_format_light_1x, 25),
    "format-light-8x": (work_format_light_8x, 4),
    "parse-single": (work_parse_single, 3000),
    "format-single": (work_format_single, 2000),
}


def run_wall(name, iters):
    import sqlparse

    fn, _ = WORKLOADS[name]
    fn(sqlparse)  # warmup
    best = None
    t0 = time.perf_counter()
    for _ in range(iters):
        fn(sqlparse)
    dt = time.perf_counter() - t0
    # best-of-3 outer for stability
    for _ in range(2):
        t0 = time.perf_counter()
        for _ in range(iters):
            fn(sqlparse)
        dt = min(dt, time.perf_counter() - t0)
    return dt / iters


def run_rss(name, iters):
    import sqlparse

    fn, _ = WORKLOADS[name]
    for _ in range(iters):
        fn(sqlparse)
    peak_kb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    print(f"{peak_kb}")


def baseline_rss():
    import sqlparse  # noqa: F401

    print(f"{resource.getrusage(resource.RUSAGE_SELF).ru_maxrss}")


def main(argv):
    if "--work" in argv:
        i = argv.index("--work")
        name, side, iters = argv[i + 1], argv[i + 2], int(argv[i + 3])
        if side == "pure":
            os.environ["SQLPARSE_PURE"] = "1"
        if name == "__baseline__":
            return baseline_rss()
        if "--rss" in argv:
            return run_rss(name, iters)
        dt = run_wall(name, 1)
        print(f"{dt * 1000:.3f}")
        return None
    if "--rss" in argv:
        rows = [("workload", "pure_peak_kb", "rust_peak_kb")]
        base = {}
        for side in ("pure", "rust"):
            env = dict(os.environ)
            if side == "pure":
                env["SQLPARSE_PURE"] = "1"
            out = subprocess.run(
                [sys.executable, __file__, "--work", "__baseline__", side, "0"],
                capture_output=True, text=True, env=env, check=True,
            )
            base[side] = int(out.stdout.strip())
        print(f"baseline peak RSS KiB: pure={base['pure']} rust={base['rust']}")
        for name, (_, iters) in WORKLOADS.items():
            cells = [name]
            for side in ("pure", "rust"):
                env = dict(os.environ)
                if side == "pure":
                    env["SQLPARSE_PURE"] = "1"
                out = subprocess.run(
                    [sys.executable, __file__, "--work", name, side, str(max(iters * 3, 5)), "--rss"],
                    capture_output=True, text=True, env=env, check=True,
                )
                cells.append(out.stdout.strip())
            rows.append(tuple(cells))
        widths = [max(len(r[i]) for r in rows) for i in range(3)]
        for r in rows:
            print("  ".join(c.ljust(w) for c, w in zip(r, widths)))
        return None
    print(f"{'workload':22} {'pure_ms':>10} {'rust_ms':>10} {'speedup':>8}")
    for name, (_, iters) in WORKLOADS.items():
        cells = []
        for side in ("pure", "rust"):
            env = dict(os.environ)
            if side == "pure":
                env["SQLPARSE_PURE"] = "1"
            out = subprocess.run(
                [sys.executable, __file__, "--work", name, side, str(iters)],
                capture_output=True, text=True, env=env, check=True,
            )
            cells.append(float(out.stdout.strip()))
        pure_ms, rust_ms = cells
        print(f"{name:22} {pure_ms:10.3f} {rust_ms:10.3f} {pure_ms / rust_ms:8.2f}x")


if __name__ == "__main__":
    main(sys.argv)
