"""Deterministic HTML fixtures shared by every engine in the benchmark."""
import os, random

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")


def product(i, rng, changed=False):
    price = f"{rng.randint(5, 500)}.{rng.randint(0, 99):02d}"
    rating = rng.choice(["One", "Two", "Three", "Four", "Five"])
    title = f"Product {i} " + " ".join(rng.choice(["Blue", "Steel", "Quiet", "Rapid", "Solar", "Prime", "Nordic", "Compact"]) for _ in range(3))
    if not changed:
        return (f'<article class="product card" id="p{i}" data-sku="SKU-{i:05d}">'
                f'<a href="/item/{i}" class="thumb"><img src="/img/{i}.jpg" alt="{title}"></a>'
                f'<h3 class="title"><a href="/item/{i}" title="{title}">{title}</a></h3>'
                f'<p class="star-rating {rating}">{rating} stars</p>'
                f'<div class="pricing"><span class="price" data-currency="USD">{price}</span>'
                f'<span class="stock">In stock</span></div>'
                f'<button class="add" type="button">Add to basket</button></article>')
    # Site redesign: renamed classes, extra wrapper, attribute renamed, order kept.
    return (f'<li class="tile" id="p{i}"><div class="tile-inner" data-sku="SKU-{i:05d}">'
            f'<a href="/item/{i}" class="media"><img src="/img/{i}.jpg" alt="{title}"></a>'
            f'<h3 class="name"><a href="/item/{i}" title="{title}">{title}</a></h3>'
            f'<p class="rating {rating}">{rating} stars</p>'
            f'<div class="cost-box"><span class="amount" data-currency="USD">{price}</span>'
            f'<span class="availability">In stock</span></div>'
            f'<button class="add" type="button">Add to basket</button></div></li>')


def page(n, changed=False):
    rng = random.Random(42)
    items = "".join(product(i, rng, changed) for i in range(n))
    container = f'<ul class="grid">{items}</ul>' if changed else f'<section class="products">{items}</section>'
    return ("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Benchmark catalogue</title>"
            "<meta name=\"description\" content=\"Synthetic catalogue for parser benchmarks\">"
            "<style>.product{display:block}.price{color:#333}</style>"
            "<script>window.analytics=[];for(let i=0;i<10;i++){analytics.push(i)}</script></head>"
            f"<body><header><nav><a href=\"/\">Home</a><a href=\"/cat\">Catalogue</a></nav></header><main>{container}</main>"
            "<footer><p>Footer text</p></footer></body></html>")


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for name, html in [("products_1k.html", page(1000)), ("products_5k.html", page(5000)),
                       ("products_1k_changed.html", page(1000, changed=True))]:
        with open(os.path.join(OUT, name), "w", encoding="utf-8", newline="\n") as f:
            f.write(html)
        print(name, len(html.encode()), "bytes")
