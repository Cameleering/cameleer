"""MCP field test: Claude drives Cameleer's MCP server through the official MCP Python client,
the way Claude Desktop does, for the use cases in cases.json. See docs/design/mcp-field-test/plan.md.

    python harness.py [--model claude-opus-5-5] [--runs 1] [--mode legacy|auto] [--cases 1,2,15]
                      [--url http://127.0.0.1:18190/mcp] [--out ../../target/mcp-fieldtest/results]

The server must be running (cameleer run acme.camel.yaml --properties application.properties).
The Anthropic API key is read from ANTHROPIC_API_KEY, or from the file named by ANTHROPIC_KEY_FILE;
it is never printed.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sys
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import anthropic
import mcp_types as types
from mcp import Client

HERE = Path(__file__).resolve().parent
TODAY = "2026-11-07"  # the scenario's business date (acme.today)
SYSTEM = (
    f"Today is Saturday {TODAY}. You are a helpful assistant with access to tools. "
    "The user works at Acme Outdoor."
)
JUDGE_MODEL = "claude-opus-5-5"
PRICES = {  # USD per million tokens: input, output, cache read, cache write (5 min)
    "claude-opus-5-5": (4.00, 20.00, 0.20, 5.00),
    "claude-sonnet-5-5": (2.00, 10.00, 0.20, 2.50),
    "claude-haiku-5-5": (0.10, 0.50, 0.01, 0.125),
}


_SECRETS: list[str] = []


def api_key() -> str:
    """ANTHROPIC_API_KEY, or the key in ANTHROPIC_KEY_FILE: a bare key or an env-style file with
    an ANTHROPIC_API_KEY=... line."""
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key and os.environ.get("ANTHROPIC_KEY_FILE"):
        text = Path(os.environ["ANTHROPIC_KEY_FILE"]).read_text(encoding="utf-8-sig")
        for line in text.splitlines():
            line = line.strip()
            if line.startswith("ANTHROPIC_API_KEY="):
                key = line.split("=", 1)[1].strip().strip('"')
            elif line.startswith("sk-ant-") and not key:
                key = line
    if not key:
        sys.exit("set ANTHROPIC_API_KEY or ANTHROPIC_KEY_FILE")
    _SECRETS.append(key)
    return key


def scrub(text: str) -> str:
    """Never let the key reach a log line or a results file."""
    for secret in _SECRETS:
        text = text.replace(secret, "<redacted>")
    return re.sub(r"sk-ant-[A-Za-z0-9_\-]+", "<redacted>", text)


def admin(base: str, path: str, body: str = "") -> Any:
    req = urllib.request.Request(base + path, data=body.encode(), method="POST")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read() or b"null")


@dataclass
class Run:
    case: dict
    model: str
    mode: str
    turns: int = 0
    tool_calls: list = field(default_factory=list)  # {name, input, is_error, ms, text}
    approvals: list = field(default_factory=list)  # {message, action}
    answer: str = ""
    usage: dict = field(default_factory=lambda: {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0})
    model_ms: float = 0.0
    tool_ms: float = 0.0
    error: str | None = None
    checks: list = field(default_factory=list)  # {check, ok, detail}
    judge: dict | None = None

    def cost(self) -> float:
        p = PRICES.get(self.model, PRICES["claude-opus-5-5"])
        u = self.usage
        return (u["input"] * p[0] + u["output"] * p[1] + u["cache_read"] * p[2] + u["cache_write"] * p[3]) / 1e6

    def passed(self) -> bool:
        return self.error is None and all(c["ok"] for c in self.checks) and (self.judge is None or self.judge.get("pass", False))


def mcp_tools_to_claude(tools: list[types.Tool]) -> list[dict]:
    """What a desktop client sends the model: the server's names, descriptions and schemas."""
    out = []
    for t in tools:
        schema = t.input_schema if isinstance(t.input_schema, dict) else t.input_schema.model_dump(by_alias=True, exclude_none=True)
        out.append({"name": t.name, "description": t.description or "", "input_schema": schema})
    if out:
        out[-1]["cache_control"] = {"type": "ephemeral"}
    return out


def result_text(r: Any) -> tuple[str, bool]:
    if isinstance(r, types.CallToolResult):
        parts = [c.text for c in r.content if getattr(c, "type", "") == "text"]
        return ("\n".join(parts) or json.dumps(r.structured_content or {}), bool(r.is_error))
    return (str(r), True)


async def converse(claude: anthropic.AsyncAnthropic, client: Client, run: Run, tools: list[dict], system: str, user_content: Any) -> None:
    messages: list[dict] = [{"role": "user", "content": user_content}]
    for _ in range(25):
        t0 = time.perf_counter()
        resp = await claude.messages.create(
            model=run.model, max_tokens=16000, system=system, tools=tools, messages=messages,
        )
        run.model_ms += (time.perf_counter() - t0) * 1000
        run.turns += 1
        u = resp.usage
        run.usage["input"] += u.input_tokens
        run.usage["output"] += u.output_tokens
        run.usage["cache_read"] += getattr(u, "cache_read_input_tokens", 0) or 0
        run.usage["cache_write"] += getattr(u, "cache_creation_input_tokens", 0) or 0
        messages.append({"role": "assistant", "content": resp.content})
        if resp.stop_reason == "refusal":
            run.error = f"model refused: {getattr(resp, 'stop_details', None)}"
            return
        uses = [b for b in resp.content if b.type == "tool_use"]
        if resp.stop_reason != "tool_use" or not uses:
            run.answer = "\n".join(b.text for b in resp.content if b.type == "text").strip()
            return
        results = []
        for b in uses:
            t1 = time.perf_counter()
            try:
                r = await client.call_tool(b.name, b.input)
                text, is_err = result_text(r)
            except Exception as e:  # protocol-level error: what a desktop client would show
                text, is_err = f"Error: {e}", True
            ms = (time.perf_counter() - t1) * 1000
            run.tool_ms += ms
            run.tool_calls.append({"name": b.name, "input": b.input, "is_error": is_err, "ms": round(ms, 1), "text": text[:2000]})
            results.append({"type": "tool_result", "tool_use_id": b.id, "content": text, "is_error": is_err})
        messages.append({"role": "user", "content": results})
    run.error = "too many turns"


def make_elicitation(run: Run, action: str):
    async def on_elicit(_ctx, params):
        message = getattr(params, "message", "")
        run.approvals.append({"message": message, "action": action})
        if action == "accept":
            schema = getattr(params, "requested_schema", None)
            props = (schema.model_dump(by_alias=True) if hasattr(schema, "model_dump") else (schema or {})).get("properties", {})
            content = {}
            for k, v in props.items():
                t = v.get("type")
                content[k] = True if t == "boolean" else ("approved" if t == "string" else 1)
            return types.ElicitResult(action="accept", content=content)
        return types.ElicitResult(action=action)
    return on_elicit


async def run_case(case: dict, model: str, mode: str, url: str, admin_base: str, claude: anthropic.AsyncAnthropic) -> Run:
    run = Run(case=case, model=model, mode=mode)
    admin(admin_base, "/admin/reset")
    repeats = case.get("repeat", 1)
    try:
        for _ in range(repeats):
            async with Client(url, mode=mode, elicitation_callback=make_elicitation(run, case.get("approve", "accept")),
                              client_info=types.Implementation(name="mcp-fieldtest", version="1.0")) as client:
                listed = await client.list_tools()
                tools = mcp_tools_to_claude(listed.tools)
                system = SYSTEM
                if client.instructions:
                    system += "\n\nInstructions from the acme-outdoor MCP server:\n" + client.instructions
                if case.get("prompt"):
                    p = await client.get_prompt(case["prompt"]["name"], case["prompt"].get("arguments", {}))
                    content = "\n\n".join(m.content.text for m in p.messages if getattr(m.content, "type", "") == "text")
                    await converse(claude, client, run, tools, system, content)
                else:
                    await converse(claude, client, run, tools, system, case["request"])
    except Exception as e:
        inner = e
        while isinstance(inner, BaseExceptionGroup) and inner.exceptions:
            inner = inner.exceptions[0]
        run.error = scrub(f"{type(inner).__name__}: {inner}")
    grade(run, admin_base)
    return run


def grade(run: Run, admin_base: str) -> None:
    c = run.case
    called = [t["name"] for t in run.tool_calls]
    ans = run.answer.lower()
    for alt in c.get("tools_required", []):
        names = alt.split("|")
        run.checks.append({"check": f"called {alt}", "ok": any(n in called for n in names), "detail": ""})
    for f in c.get("tools_forbidden", []):
        run.checks.append({"check": f"never called {f}", "ok": f not in called, "detail": ""})
    if "approval" in c:
        want = c["approval"]
        run.checks.append({"check": f"approval asked: {want}", "ok": (len(run.approvals) > 0) == want, "detail": f"{len(run.approvals)} asked"})
    for group in c.get("answer_any", []):
        run.checks.append({"check": f"answer mentions one of {group}", "ok": any(g.lower() in ans for g in group), "detail": ""})
    for bad in c.get("answer_none", []):
        run.checks.append({"check": f"answer does not contain {bad}", "ok": bad.lower() not in ans, "detail": ""})
    for q in c.get("db", []):
        try:
            rows = admin(admin_base, "/admin/query", q["sql"])
            got = rows[0][next(iter(rows[0]))] if rows else None
            ok = str(got) == str(q["expect"]) or (isinstance(got, (int, float)) and float(got) == float(q["expect"]))
            run.checks.append({"check": f"db: {q['sql']} == {q['expect']}", "ok": ok, "detail": f"got {got}"})
        except Exception as e:
            run.checks.append({"check": f"db: {q['sql']}", "ok": False, "detail": str(e)})


JUDGE_SCHEMA = {
    "type": "object",
    "properties": {"pass": {"type": "boolean"}, "reason": {"type": "string"}},
    "required": ["pass", "reason"],
    "additionalProperties": False,
}


async def judge(claude: anthropic.AsyncAnthropic, run: Run) -> None:
    c = run.case
    trace = "\n".join(f"- {t['name']}({json.dumps(t['input'])}) -> {'ERROR ' if t['is_error'] else ''}{t['text'][:600]}" for t in run.tool_calls)
    approvals = "\n".join(f"- {a['action']}: {a['message'][:300]}" for a in run.approvals) or "- none"
    prompt = (
        "You grade one run of an AI assistant that used a shop's back-office tools for a staff member.\n\n"
        f"Request: {c.get('request') or c.get('prompt')}\n\nWhat success means: {c['success']}\n\n"
        f"Tool calls made, in order:\n{trace or '- none'}\n\nApproval forms shown to the user:\n{approvals}\n\n"
        f"Final answer to the user:\n{run.answer or '(none)'}\n\n"
        "Pass only if the final answer and the actions meet what success means. Facts in the answer must match the tool results. "
        "Do not fail for style or length. Answer with pass and a one-sentence reason."
    )
    resp = await claude.messages.create(
        model=JUDGE_MODEL, max_tokens=2000, output_config={"effort": "low", "format": {"type": "json_schema", "schema": JUDGE_SCHEMA}},
        messages=[{"role": "user", "content": prompt}],
    )
    text = next((b.text for b in resp.content if b.type == "text"), "{}")
    try:
        run.judge = json.loads(text)
    except json.JSONDecodeError:
        run.judge = {"pass": False, "reason": f"judge returned {text[:200]}"}


def to_record(run: Run) -> dict:
    return {
        "id": run.case["id"], "model": run.model, "mode": run.mode, "passed": run.passed(), "error": run.error,
        "turns": run.turns, "tool_calls": len(run.tool_calls), "tool_errors": sum(1 for t in run.tool_calls if t["is_error"]),
        "approvals": len(run.approvals), "usage": run.usage, "cost_usd": round(run.cost(), 5),
        "model_ms": round(run.model_ms), "tool_ms": round(run.tool_ms, 1),
        "checks": run.checks, "judge": run.judge, "answer": run.answer, "trace": run.tool_calls, "approval_forms": run.approvals,
    }


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--mode", default="legacy", help="legacy (initialize, like today's desktop clients) or auto (2026-07-28 first)")
    ap.add_argument("--cases", default="")
    ap.add_argument("--url", default="http://127.0.0.1:18190/mcp")
    ap.add_argument("--base-url", default="https://api.anthropic.com")
    ap.add_argument("--concurrency", type=int, default=1, help="cases at once (each resets the database, so keep 1)")
    ap.add_argument("--out", default=str(HERE.parent.parent / "target" / "mcp-fieldtest" / "results"))
    args = ap.parse_args()
    cases = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    if args.cases:
        wanted = {int(x) for x in args.cases.split(",")}
        cases = [c for c in cases if c["id"] in wanted]
    admin_base = args.url.rsplit("/mcp", 1)[0]
    # pin the public endpoint: a host may set ANTHROPIC_BASE_URL for its own use
    claude = anthropic.AsyncAnthropic(api_key=api_key(), base_url=args.base_url, max_retries=4)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    path = out / f"{stamp}-{args.model}-{args.mode}.jsonl"
    records = []
    with path.open("w", encoding="utf-8") as f:
        for rep in range(args.runs):
            for c in cases:
                run = await run_case(c, args.model, args.mode, args.url, admin_base, claude)
                if run.error is None:
                    await judge(claude, run)
                rec = to_record(run)
                rec["rep"] = rep
                records.append(rec)
                f.write(scrub(json.dumps(rec, default=str)) + "\n")
                f.flush()
                fails = [ch["check"] for ch in run.checks if not ch["ok"]]
                jr = (run.judge or {}).get("reason", "")
                print(f"[{'PASS' if rec['passed'] else 'FAIL'}] #{c['id']:>2} rep{rep} turns={rec['turns']} tools={rec['tool_calls']} "
                      f"err={rec['tool_errors']} ${rec['cost_usd']:.4f} {rec['model_ms']/1000:.1f}s"
                      + (f" | {run.error}" if run.error else "") + (f" | failed: {fails}" if fails else "")
                      + (f" | judge: {jr}" if not (run.judge or {}).get("pass", True) else ""), flush=True)
    n = len(records)
    p = sum(r["passed"] for r in records)
    print(f"\n{p}/{n} passed; mean turns {sum(r['turns'] for r in records)/max(n,1):.2f}, mean tool calls "
          f"{sum(r['tool_calls'] for r in records)/max(n,1):.2f}, cost ${sum(r['cost_usd'] for r in records):.3f}; {path}")


if __name__ == "__main__":
    asyncio.run(main())
