"""Time the station search/detail/return journey against a loopback emob demo."""

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from html.parser import HTMLParser
from http.client import HTTPConnection
from math import ceil
from pathlib import Path
from time import perf_counter
from urllib.parse import parse_qs, urlsplit


class Links(HTMLParser):
    def __init__(self):
        super().__init__()
        self.hrefs = []
        self.in_main = False

    def handle_starttag(self, tag, attrs):
        if tag == "main":
            self.in_main = True
        elif self.in_main and tag == "a":
            self.hrefs.append(dict(attrs).get("href", ""))

    def handle_endtag(self, tag):
        if tag == "main":
            self.in_main = False

    def link(self, path):
        return next(href for href in self.hrefs if urlsplit(href).path == path)


def measure(estate, headers):
    def page(path):
        # Pin every request to this demo, including links read from its HTML.
        connection = HTTPConnection("127.0.0.1", estate.port, timeout=10)
        try:
            connection.request("GET", path, headers=headers)
            response = connection.getresponse()
            html = response.read().decode()
            if response.status != 200:
                raise ValueError(f"demo returned HTTP {response.status}")
        finally:
            connection.close()
        links = Links()
        links.feed(html)
        return links

    started = perf_counter()
    search = "/stations?q=CP-3&lang=en"
    station = page(search).link("/station/CP-3")
    results = page(station).link("/stations")
    if parse_qs(urlsplit(results).query) != parse_qs(urlsplit(search).query):
        raise ValueError("station detail lost the search")
    if page(results).link("/station/CP-3") != station:
        raise ValueError("returned list lost the station")
    return (perf_counter() - started) * 1000


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("links", type=Path, help="file written by emob demo --links")
    args = parser.parse_args()
    values = dict(line.split("=", 1) for line in args.links.read_text().splitlines())
    estate = urlsplit(values["heute"])
    if estate.scheme != "http" or estate.hostname != "127.0.0.1" or not estate.port:
        parser.error("expected the local demo desk")
    token = base64.b64encode(f"demo:{values['token']}".encode()).decode()
    headers = {"Authorization": f"Basic {token}"}
    measure(estate, headers)
    for workers in (1, 16):
        with ThreadPoolExecutor(workers) as pool:
            futures = [pool.submit(measure, estate, headers) for _ in range(100)]
            times = sorted(future.result() for future in futures)
        p95 = times[ceil(0.95 * len(times)) - 1]
        print(f"{workers:2} workers · {len(times)} correct journeys · "
              f"p50 {times[49]:.1f} ms · p95 {p95:.1f} ms")


if __name__ == "__main__":
    main()
