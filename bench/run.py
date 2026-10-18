"""Benchmark orchestrator. Run from the repository root after building the Rust runner:

    cargo build --release -p ozyscrape --example compare
    python bench/make_fixtures.py
    python -m venv bench/venv && bench/venv/Scripts/pip install scrapling beautifulsoup4 lxml parsel psutil
    bench/venv/Scripts/python bench/run.py          (bench/venv/bin/python on Linux/macOS)

Writes bench/results.json. Every number in BENCHMARKS.md comes from that file.
"""
import json
import os
import platform
import statistics
import subprocess
import sys
import time

import psutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIX = os.path.join(ROOT, "bench", "fixtures")
RUST = os.path.join(ROOT, "target", "release", "examples", "compare" + (".exe" if os.name == "nt" else ""))
PY = sys.executable
PYBENCH = os.path.join(ROOT, "bench", "python_bench.py")
RUST_ENGINES = ["ozyscrape", "scraper", "tl"]
PY_ENGINES = ["scrapling", "lxml", "parsel", "bs4_lxml", "bs4_htmlparser"]
PROCESS_REPEATS = 3
TINY = "<!doctype html><html><body><section class=\"products\"><article class=\"product\"><span class=\"price\">1</span></article></section></body></html>"


def command(engine, task, fixture, iterations, extra=()):
    path = os.path.join(FIX, fixture)
    if engine in RUST_ENGINES:
        return [RUST, engine, task, path, str(iterations), *extra]
    return [PY, "-I", PYBENCH, engine, task, path, str(iterations), *extra]


def run(cmd):
    """Runs one benchmark process; returns its JSON line and peak memory in MiB."""
    proc = psutil.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    peak = 0
    # On Windows a venv's python.exe is a launcher that runs the real interpreter as a
    # child process, so the whole process tree is sampled and the largest peak is kept.
    while proc.poll() is None:
        try:
            for member in [proc, *proc.children(recursive=True)]:
                info = member.memory_info()
                peak = max(peak, getattr(info, "peak_wset", 0) or info.rss)
        except psutil.Error:
            pass
        time.sleep(0.005)
    out, err = proc.communicate()
    if proc.returncode != 0:
        raise RuntimeError(f"{cmd} failed: {err[-800:]}")
    return json.loads(out.strip().splitlines()[-1]), peak / (1024 * 1024)


def measure(engine, task, fixture, iterations, extra=()):
    medians, last = [], None
    for _ in range(PROCESS_REPEATS):
        result, _ = run(command(engine, task, fixture, iterations, extra))
        last = result["result"]
        if "skipped" in last:
            return last
        medians.append(last.get("timing", last)["median_ms"])
    out = dict(last)
    out["median_of_medians_ms"] = round(statistics.median(medians), 3)
    out["process_medians_ms"] = [round(m, 3) for m in medians]
    return out


def memory_row(engine, fixture):
    """Peak working set while holding the parsed DOM, minus a tiny-document baseline (median of 3)."""
    peaks = [run(command(engine, "memory", fixture, 1))[1] for _ in range(PROCESS_REPEATS)]
    bases = [run(command(engine, "memory", "tiny.html", 1))[1] for _ in range(PROCESS_REPEATS)]
    peak, base = statistics.median(peaks), statistics.median(bases)
    return {"peak_mib": round(peak, 1), "baseline_mib": round(base, 1), "dom_delta_mib": round(peak - base, 1)}


def memory_only():
    path = os.path.join(ROOT, "bench", "results.json")
    with open(path, encoding="utf-8") as f:
        results = json.load(f)
    for fixture, entry in results["fixtures"].items():
        for engine, row in entry["engines"].items():
            row["memory"] = memory_row(engine, fixture)
            print(fixture, engine, "memory", row["memory"], flush=True)
    results["memory_method"] = "psutil peak working set of the benchmark process tree, median of 3 runs, minus tiny-document baseline"
    with open(path, "w", encoding="utf-8") as f:
        json.dump(results, f, indent=2)
    print("updated memory in bench/results.json")


def main():
    with open(os.path.join(FIX, "tiny.html"), "w", encoding="utf-8") as f:
        f.write(TINY)
    if "--memory-only" in sys.argv:
        return memory_only()
    env = {
        "os": platform.platform(),
        "cpu": platform.processor(),
        "logical_cores": psutil.cpu_count(logical=True),
        "python": platform.python_version(),
        "rustc": subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip(),
        "date": time.strftime("%Y-%m-%d %H:%M:%S"),
    }
    try:
        from importlib.metadata import version
        env["versions"] = {p: version(p) for p in ["scrapling", "beautifulsoup4", "lxml", "parsel"]}
    except Exception:
        pass
    results = {"environment": env, "selector": ".product .price (tl: .price)", "process_repeats": PROCESS_REPEATS, "fixtures": {}}
    for fixture, iterations in [("products_1k.html", 30), ("products_5k.html", 10)]:
        entry = {"bytes": os.path.getsize(os.path.join(FIX, fixture)), "engines": {}}
        for engine in RUST_ENGINES + PY_ENGINES:
            row = {}
            for task in ["parse", "extract", "parse_extract"]:
                row[task] = measure(engine, task, fixture, iterations)
                print(fixture, engine, task, row[task].get("median_of_medians_ms", row[task]), flush=True)
            row["memory"] = memory_row(engine, fixture)
            print(fixture, engine, "memory", row["memory"], flush=True)
            entry["engines"][engine] = row
        results["fixtures"][fixture] = entry
    adaptive = {}
    for engine in ["ozyscrape", "scrapling"]:
        adaptive[engine] = measure(engine, "adaptive", "products_1k.html", 20, (os.path.join(FIX, "products_1k_changed.html"),))
        print("adaptive", engine, adaptive[engine], flush=True)
    results["adaptive"] = adaptive
    with open(os.path.join(ROOT, "bench", "results.json"), "w", encoding="utf-8") as f:
        json.dump(results, f, indent=2)
    print("wrote bench/results.json")


if __name__ == "__main__":
    main()
