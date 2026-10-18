"""Python side of the benchmark (comparison only; never a runtime dependency).

    python python_bench.py <engine> <task> <file> <iterations> [changed-file]
    engines: scrapling | bs4_htmlparser | bs4_lxml | lxml | parsel
"""
import gc
import json
import os
import sys
import tempfile
import time

SELECTOR = ".product .price"


def stats(samples):
    samples = sorted(samples)

    def pick(q):
        return samples[round((len(samples) - 1) * q)]

    return {"median_ms": pick(0.5), "p95_ms": pick(0.95), "min_ms": samples[0], "runs": len(samples)}


def timed(iterations, fn):
    for _ in range(3):
        fn()
    samples, last = [], None
    for _ in range(iterations):
        start = time.perf_counter()
        last = fn()
        samples.append((time.perf_counter() - start) * 1000)
    return stats(samples), last


def engine_fns(engine):
    if engine == "scrapling":
        from scrapling.parser import Selector
        return (lambda html: Selector(html), lambda doc: [e.text for e in doc.css(SELECTOR)])
    if engine.startswith("bs4"):
        from bs4 import BeautifulSoup
        parser = "html.parser" if engine == "bs4_htmlparser" else "lxml"
        return (lambda html: BeautifulSoup(html, parser), lambda doc: [e.get_text() for e in doc.select(SELECTOR)])
    if engine == "lxml":
        import lxml.html
        return (lambda html: lxml.html.fromstring(html), lambda doc: [e.text_content() for e in doc.cssselect(SELECTOR)])
    if engine == "parsel":
        import parsel
        return (lambda html: parsel.Selector(text=html), lambda doc: doc.css(SELECTOR + "::text").getall())
    raise SystemExit(f"unknown engine {engine}")


def adaptive_scrapling(html, changed, iterations):
    from scrapling.parser import Selector
    workdir = tempfile.mkdtemp(prefix="scrapling-bench-")
    args = {"storage_file": os.path.join(workdir, "adaptive.db"), "url": "https://bench.local/"}
    original = Selector(html, url="https://bench.local/", adaptive=True, storage_args=args)
    expected = str(original.css("#p500 .price", auto_save=True)[0].text)
    moved = Selector(changed, url="https://bench.local/", adaptive=True, storage_args=args)
    timing, found = timed(iterations, lambda: moved.css("#p500 .price", adaptive=True))
    value = str(found[0].text) if found else ""
    return {"timing": timing, "expected": expected, "found": value, "correct": value == expected, "candidates": len(found)}


def main():
    engine, task, path = sys.argv[1], sys.argv[2], sys.argv[3]
    iterations = int(sys.argv[4]) if len(sys.argv) > 4 else 20
    with open(path, encoding="utf-8") as f:
        html = f.read()
    if task == "adaptive":
        if engine != "scrapling":
            result = {"skipped": "no adaptive selector support"}
        else:
            with open(sys.argv[5], encoding="utf-8") as f:
                result = adaptive_scrapling(html, f.read(), iterations)
    else:
        parse, extract = engine_fns(engine)
        if task == "parse":
            result, _ = timed(iterations, lambda: parse(html))
        elif task == "extract":
            doc = parse(html)
            timing, values = timed(iterations, lambda: extract(doc))
            result = {"timing": timing, "matches": len(values)}
        elif task == "parse_extract":
            timing, values = timed(iterations, lambda: extract(parse(html)))
            result = {"timing": timing, "matches": len(values)}
        elif task == "memory":
            doc = parse(html)
            values = extract(doc)
            gc.collect()
            time.sleep(0.4)
            result = {"matches": len(values)}
        else:
            result = {"skipped": "unknown task"}
    print(json.dumps({"engine": engine, "task": task, "result": result}))


if __name__ == "__main__":
    main()
