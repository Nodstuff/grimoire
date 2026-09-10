#!/usr/bin/env python3
"""AX 2 smoke + byte measurement against a scratch daemon (never 7425 / ~/.grimoire).

Usage:
  cargo build --release -p grimoire
  scripts/mcp-smoke.py                 # starts a scratch daemon on 7519, runs, stops it
  scripts/mcp-smoke.py --url http://127.0.0.1:7519/mcp   # against a daemon you started

Checks: edit_doc with 0/1/many matches, append to an ambiguous path (must
error), append with create_missing, read_doc(refs) round-tripping through
propose_markdown as zero ops, ?as= / ?cwd= / X-Grimoire-Principal attribution
visible in proposals(kind: mine). Prints the byte size of read_doc (default,
refs, section) and of an edit_doc and an append result.

Minimal streamable-HTTP MCP client: POST JSON-RPC to /mcp with
Accept: application/json, text/event-stream; initialize (2025-03-26), then
tools/call; answers may arrive as JSON or as `data:` SSE lines.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

PORT = 7519


class Mcp:
    def __init__(self, url, headers=None):
        self.url = url
        self.headers = headers or {}
        self.session = None
        self.seq = 0
        r = self.rpc("initialize", {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "mcp-smoke", "version": "0"},
        })
        assert "serverInfo" in r, r
        self.notify("notifications/initialized")

    def _post(self, body):
        data = json.dumps(body).encode()
        req = urllib.request.Request(self.url, data=data, method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Accept", "application/json, text/event-stream")
        if self.session:
            req.add_header("Mcp-Session-Id", self.session)
        for k, v in self.headers.items():
            req.add_header(k, v)
        with urllib.request.urlopen(req, timeout=30) as resp:
            sid = resp.headers.get("Mcp-Session-Id")
            if sid:
                self.session = sid
            ctype = resp.headers.get("Content-Type", "")
            raw = resp.read().decode()
        if not raw.strip():
            return None
        if ctype.startswith("text/event-stream"):
            msgs = []
            for l in raw.splitlines():
                if l.startswith("data:") and l[5:].strip().startswith("{"):
                    msgs.append(json.loads(l[5:].strip()))
            for m in msgs:
                if "result" in m or "error" in m:
                    return m
            return msgs[-1] if msgs else None
        return json.loads(raw)

    def notify(self, method, params=None):
        self._post({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def rpc(self, method, params):
        self.seq += 1
        m = self._post({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params})
        if m is None:
            raise RuntimeError(f"no response to {method}")
        if "error" in m:
            raise RuntimeError(f"{method}: {m['error']}")
        return m["result"]

    def call(self, name, **args):
        """→ (is_error, text)."""
        r = self.rpc("tools/call", {"name": name, "arguments": args})
        text = "".join(c.get("text", "") for c in r.get("content", []) if c.get("type") == "text")
        return bool(r.get("isError")), text

    def tools(self):
        return [t["name"] for t in self.rpc("tools/list", {})["tools"]]


FAILS = 0


def check(label, cond, detail=""):
    global FAILS
    mark = "✓" if cond else "✗"
    print(f"   {mark} {label}" + (f"  — {detail[:300]}" if (detail and not cond) else ""))
    if not cond:
        FAILS += 1


def step(s):
    print(f"\n== {s}")


DAILY = (
    "---\ntags:\n  - daily\n---\n\n"
    "## qompass\n\n### Done\n\n- shipped x\n\n### Plans\n\n- plan q\n\n"
    "## portus\n\n### Plans\n\n- plan p\n\n"
    "## grimoire\n\nintro line\n"
)


def run(base):
    url = base + "/mcp"
    m = Mcp(url)
    step("tools")
    names = m.tools()
    check("16 tools", len(names) == 16, ", ".join(sorted(names)))
    for gone in ["identify", "tree", "list_docs", "read_block", "list_comments", "my_proposals", "review_queue", "rename_doc", "backlinks"]:
        check(f"{gone} removed", gone not in names)

    step("create the fixture doc")
    err, out = m.call("create_doc", title="2026-09-10 smoke", markdown=DAILY, if_exists="error", **{"as": "claude:smoke"})
    check("create_doc one-liner", not err and out.startswith("ok · created “2026-09-10 smoke” · doc "), out)
    doc = out.split(" · doc ")[1].split(" · ")[0]

    step("read_doc byte sizes")
    sizes = {}
    err, out = m.call("read_doc", doc_id=doc)
    check("read_doc default is text with header", not err and out.startswith(f"doc {doc} · epoch "), out)
    check("read_doc body is the export", out.split("\n\n", 1)[1] == DAILY, out)
    sizes["read_doc default"] = len(out.encode())
    err, refs = m.call("read_doc", doc_id=doc, refs=True)
    check("read_doc refs has ^ lines", not err and "\n^" in refs, refs)
    sizes["read_doc refs"] = len(refs.encode())
    err, sec = m.call("read_doc", doc_id=doc, section="qompass › Plans")
    check("read_doc section", not err and sec.splitlines()[1] == "section qompass › Plans" and "### Plans\n\n- plan q\n" in sec, sec)
    sizes["read_doc section"] = len(sec.encode())
    err, out = m.call("read_doc", doc_id=doc, section="Plans")
    check("read_doc ambiguous section errors", err and "ambiguous" in out, out)
    err, out = m.call("read_doc", doc_id=doc, mode="outline")
    sizes["read_doc outline (JSON)"] = len(out.encode())

    step("refs round-trip through propose_markdown = zero ops")
    epoch = int(refs.splitlines()[0].split(" · epoch ")[1].split(" · ")[0])
    body = refs.split("\n\n", 1)[1]
    err, out = m.call("propose_markdown", doc_id=doc, base_epoch=epoch, markdown=body, **{"as": "claude:smoke"})
    check("no changes", not err and out.startswith("ok · no changes"), out)

    step("edit_doc 0 / 1 / many")
    err, out = m.call("edit_doc", doc_id=doc, old="- shipped y", new="z", **{"as": "claude:smoke"})
    check("0 matches → not found + closest", err and out.startswith("old not found") and "closest block ^" in out, out)
    err, out = m.call("edit_doc", doc_id=doc, old="### Plans", new="### Plan", **{"as": "claude:smoke"})
    check(">1 matches → lists refs", err and out.startswith("old matches 2 times") and out.count("^") == 2, out)
    err, out = m.call("edit_doc", doc_id=doc, old="- plan q", new="- plan q (done)", **{"as": "claude:smoke"})
    check("1 match → one-line verdict", not err and out.startswith("ok · 1 replace · epoch "), out)
    sizes["edit_doc result"] = len(out.encode())
    err, out = m.call("edit_doc", doc_id=doc, old="- plan q (done)", new="- plan q (done)", **{"as": "claude:smoke"})
    check("old == new → no changes", not err and out.startswith("ok · no changes"), out)
    err, out = m.call("edit_doc", doc_id=doc, old="###   Done\n- shipped x", new="### Done\n\n- shipped x, y", **{"as": "claude:smoke"})
    check("whitespace-normalised fallback", not err and out.startswith("ok · 1 replace"), out)

    step("append")
    err, out = m.call("append", doc_id=doc, markdown="- plan z", to="Plans", **{"as": "claude:smoke"})
    check("ambiguous path errors", err and "ambiguous" in out and "qompass › Plans" in out, out)
    err, out = m.call("append", doc_id=doc, markdown="- plan z", to="Plans", create_missing=True, **{"as": "claude:smoke"})
    check("ambiguous path errors even with create_missing", err and "ambiguous" in out, out)
    err, out = m.call("append", doc_id=doc, markdown="- plan q2", to="qompass › Plans", **{"as": "claude:smoke"})
    check("append to section", not err and out.startswith("ok · 1 insert · epoch ") and "\nnew: ^" in out, out)
    sizes["append result"] = len(out.encode())
    err, out = m.call("append", doc_id=doc, markdown="- plan g", to="grimoire › Plans", **{"as": "claude:smoke"})
    check("missing path without create_missing errors", err and "create_missing" in out, out)
    err, out = m.call("append", doc_id=doc, markdown="- plan g", to="grimoire › Plans", create_missing=True, **{"as": "claude:smoke"})
    check("create_missing creates the heading", not err and out.startswith("ok · 2 insert"), out)
    err, out = m.call("read_doc", doc_id=doc)
    check("doc now ends with the new section", out.endswith("intro line\n\n### Plans\n\n- plan g\n"), out[-200:])
    check("append landed before ## portus", "- plan q (done)\n\n- plan q2\n\n## portus" in out, out)

    step("attribution: as / ?as= / ?cwd= / header → proposals(kind: mine)")
    q = Mcp(url + "?as=claude:via-query")
    err, out = q.call("append", doc_id=doc, markdown="by query")
    check("?as= write ok", not err, out)
    err, out = q.call("proposals", kind="mine")
    check("?as= visible in proposals", not err and json.loads(out)["proposals"] and all(
        p["op"]["kind"].get("content") == "by query" for p in json.loads(out)["proposals"]), out)
    c = Mcp(url + "?cwd=/Users/someone/src/portus")
    err, out = c.call("append", doc_id=doc, markdown="by cwd")
    check("?cwd= write ok", not err, out)
    err, out = c.call("proposals", kind="mine")
    check("?cwd= → claude:portus visible in proposals", not err and len(json.loads(out)["proposals"]) == 1
          and json.loads(out)["proposals"][0]["op"]["kind"]["content"] == "by cwd", out)
    h = Mcp(url + "?as=claude:loser", headers={"X-Grimoire-Principal": "claude:via-header"})
    err, out = h.call("append", doc_id=doc, markdown="by header")
    check("header write ok", not err, out)
    err, out = h.call("proposals", kind="mine")
    check("header beats ?as=", not err and len(json.loads(out)["proposals"]) == 1
          and json.loads(out)["proposals"][0]["op"]["kind"]["content"] == "by header", out)
    err, out = h.call("proposals", kind="mine", **{"as": "claude:smoke"})
    check("tool `as` beats the header", not err and len(json.loads(out)["proposals"]) >= 5, out)
    # name the daemon's human, then try to act as them
    req = urllib.request.Request(base + "/api/profile", data=json.dumps({"name": "smokehuman"}).encode(), method="POST")
    req.add_header("Content-Type", "application/json")
    urllib.request.urlopen(req, timeout=10).read()
    err, out = Mcp(url, headers={"X-Grimoire-Principal": "smokehuman"}).call("append", doc_id=doc, markdown="nope")
    check("the human is refused by header", err and "not an agent" in out, out)
    err, out = m.call("append", doc_id=doc, markdown="nope", **{"as": "smokehuman"})
    check("the human is refused by `as`", err and "not an agent" in out, out)

    step("dedupe: an identical retry replays")
    err, first = m.call("append", doc_id=doc, markdown="once only", **{"as": "claude:smoke"})
    err2, again = m.call("append", doc_id=doc, markdown="once only", **{"as": "claude:smoke"})
    check("replayed verdict", not err and not err2 and first == again, again)
    err, out = m.call("read_doc", doc_id=doc)
    check("applied once", out.count("once only") == 1)

    step("bytes")
    width = max(len(k) for k in sizes)
    for k, v in sizes.items():
        print(f"   {k:<{width}}  {v:>6} B")
    return sizes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", help="an already-running daemon's base URL (e.g. http://127.0.0.1:7519); default: start a scratch one")
    args = ap.parse_args()
    if args.url:
        base = args.url.removesuffix("/mcp").rstrip("/")
        run(base)
    else:
        repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        binary = os.path.join(repo, "target", "release", "grimoire")
        if not os.path.exists(binary):
            sys.exit(f"{binary} missing: cargo build --release -p grimoire first")
        scratch = tempfile.mkdtemp(prefix="grimoire-mcp-smoke.")
        env = dict(os.environ, GRIMOIRE_IDENTITY_FILE=os.path.join(scratch, "identity.key"))
        log = open(os.path.join(scratch, "log"), "w")
        proc = subprocess.Popen([binary, "--db", os.path.join(scratch, "ks.db"), "--port", str(PORT), "serve"],
                                env=env, stdout=log, stderr=subprocess.STDOUT)
        base = f"http://127.0.0.1:{PORT}"
        try:
            for _ in range(100):
                try:
                    urllib.request.urlopen(base + "/api/buildinfo", timeout=1).read()
                    break
                except (urllib.error.URLError, ConnectionError):
                    time.sleep(0.1)
            else:
                sys.exit(f"daemon did not come up; log: {scratch}/log")
            run(base)
        finally:
            proc.terminate()
            proc.wait(timeout=10)
            log.close()
            if FAILS == 0:
                shutil.rmtree(scratch, ignore_errors=True)
            else:
                print(f"\nscratch kept at {scratch}")
    print(f"\n{'OK' if FAILS == 0 else f'{FAILS} FAILED'}")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
