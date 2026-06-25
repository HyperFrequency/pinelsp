#!/usr/bin/env python3
"""Differential oracle harness for the Pine Rust checker.

Compares `pine-cli` diagnostics against TradingView's `translate_light` for a set
of snippets, reporting error-level parity: how many TV error-lines our checker
catches, and whether we emit false-positive errors TV doesn't. This is the P3
gate — as checks are ported, `caught/total` rises and false-positives stay 0.

Run from `pine-rs/`:  python3 scripts/differential.py
"""
import json
import os
import subprocess
import tempfile
import time

# Curated cases (grow over time). Each is a (description, source) pair.
CASES = {
    "clean": '//@version=6\nindicator("x")\nplot(close)\n',
    "undefined_ident": '//@version=6\nindicator("x")\nplot(undefined_var_xyz)\n',
    "undefined_func": '//@version=6\nindicator("x")\ny = noSuchFn(1)\nplot(y)\n',
    "missing_version": 'indicator("x")\nplot(close)\n',
    "unused_var": '//@version=6\nindicator("x")\nunused = 42\nplot(close)\n',
    "type_mismatch": '//@version=6\nindicator("x")\nint a = "hello"\nplot(close)\n',
    "unknown_arg": '//@version=6\nindicator("x")\nplot(close, badparam=1)\n',
    "too_many_args": '//@version=6\nindicator("x")\nx = math.abs(1, 2, 3)\nplot(x)\n',
    "missing_arg": '//@version=6\nindicator("x")\nx = ta.sma(close)\nplot(x)\n',
    "na_compare": '//@version=6\nindicator("x")\nb = close == na\nplot(close)\n',
}

TV_URL = ("https://pine-facade.tradingview.com/pine-facade/translate_light"
          "?user_name=admin&v=3")


def tv_errors(src):
    """TradingView error lines (1-based) + messages, via curl multipart POST."""
    r = subprocess.run(
        ["curl", "-s", "--max-time", "25", "-X", "POST", TV_URL,
         "-H", "Referer: https://www.tradingview.com/",
         "-H", "User-Agent: Mozilla/5.0", "-H", "DNT: 1",
         "-F", "source=" + src],
        capture_output=True, text=True)
    try:
        d = json.loads(r.stdout)
        return [(e["start"]["line"], e.get("message", "")) for e in
                d.get("result", {}).get("errors", [])]
    except Exception:
        return None  # network/parse failure → distinguish from "no errors"


def my_diags(src):
    """pine-cli diagnostics as (line_1based, severity, code)."""
    with tempfile.NamedTemporaryFile("w", suffix=".pine", delete=False) as f:
        f.write(src)
        path = f.name
    try:
        r = subprocess.run(["target/debug/pine-cli", "--json", path],
                           capture_output=True, text=True)
        d = json.loads(r.stdout)
        return [(x["range"]["start"]["line"] + 1, x["severity"], x["code"])
                for x in d["diagnostics"]]
    except Exception:
        return []
    finally:
        os.unlink(path)


def main():
    subprocess.run(["cargo", "build", "-q", "-p", "pine-cli"], check=True)
    tv_total = caught = false_pos = 0
    print(f"{'case':18} {'TV err lines':18} {'pine-cli diags'}")
    for name, src in CASES.items():
        tv = tv_errors(src)
        mine = my_diags(src)
        if tv is None:
            print(f"{name:18} <oracle unreachable>")
            continue
        tv_lines = {ln for ln, _ in tv}
        my_err_lines = {ln for (ln, sev, _) in mine if sev == "error"}
        tv_total += len(tv_lines)
        caught += len(tv_lines & my_err_lines)
        false_pos += len(my_err_lines - tv_lines)
        mine_str = ",".join(f"{c}@{ln}" for (ln, _, c) in mine) or "-"
        print(f"{name:18} {str(sorted(tv_lines)):18} {mine_str}")
        time.sleep(0.5)
    print(f"\nERROR PARITY: caught {caught}/{tv_total} TV error-lines; "
          f"false-positive error-lines: {false_pos}")


if __name__ == "__main__":
    main()
