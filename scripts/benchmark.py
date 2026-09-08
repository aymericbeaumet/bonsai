#!/usr/bin/env python3
"""Benchmark release builds against disposable, deterministic local fixtures.

    cargo build --release --locked
    python3 scripts/benchmark.py > benchmark.jsonl
    python3 scripts/benchmark.py --worktrees 1 10 --sessions 1000 --runs 3

Uses only Python's standard library, Git, and the selected Bonsai executable.
No network access, dependency installation, or personal session stores are used.
All linked checkouts are created and removed through Bonsai. Default fixtures
include 100,000 tiny rollout files; use smaller --sessions values for a smoke run.

Each JSON line reports the first invocation separately from warm median/p95 and
raw samples. "First" does not claim an OS-cold cache: this script never drops
system caches. A separate untimed Git Trace2 pass counts Git process starts;
this excludes non-Git processes, bytes read, and peak memory. Use an OS profiler
alongside this script when those measurements are needed. Results are diagnostic
baselines, not portable CI latency thresholds.
"""

import argparse
import json
import math
import os
from pathlib import Path
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
import uuid


def run(command, cwd, env, timeout):
    result = subprocess.run(
        command, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
    )
    if result.returncode:
        raise RuntimeError(
            f"{command!r} exited {result.returncode}:\n{result.stderr[-4000:]}"
        )
    return result


def measure(binary, command, fixture, env, args, workload, size):
    samples = []
    invocation = [str(binary), *command]
    for _ in range(args.runs + 1):
        started = time.perf_counter()
        run(invocation, fixture, env, args.timeout)
        samples.append((time.perf_counter() - started) * 1000)
    first, *warm = samples
    record = {
        "workload": workload,
        "size": size,
        "command": command,
        "first_ms": round(first, 2),
        "warm_median_ms": round(statistics.median(warm), 2),
        "warm_p95_ms": round(sorted(warm)[math.ceil(len(warm) * 0.95) - 1], 2),
        "warm_samples_ms": [round(value, 2) for value in warm],
    }
    if not args.no_trace:
        trace = fixture.parent / "git-trace.jsonl"
        trace.unlink(missing_ok=True)
        run(invocation, fixture, dict(env, GIT_TRACE2_EVENT=str(trace)), args.timeout)
        record["git_processes"] = sum(
            json.loads(line).get("event") == "start"
            for line in trace.read_text().splitlines()
        ) if trace.exists() else 0
    print(json.dumps(record), flush=True)


def environment(root):
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("BONSAI_", "_BONSAI_", "GIT_", "CODEX_"))
    }
    empty_config = root / "empty.gitconfig"
    empty_config.touch()
    env.update(
        BONSAI_ROOT=str(root / "worktrees"),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_DATA_HOME=str(root / "data"),
        CLAUDE_CONFIG_DIR=str(root / "claude"),
        CODEX_HOME=str(root / "codex"),
        CODEX_SQLITE_HOME=str(root / "codex"),
        OPENCODE_DB=str(root / "opencode.db"),
        GIT_CONFIG_GLOBAL=str(empty_config),
        GIT_CONFIG_NOSYSTEM="1",
        GIT_TERMINAL_PROMPT="0",
        GIT_AUTHOR_NAME="Benchmark",
        GIT_COMMITTER_NAME="Benchmark",
        GIT_AUTHOR_EMAIL="benchmark@example.invalid",
        GIT_COMMITTER_EMAIL="benchmark@example.invalid",
    )
    fake_bin = root / "bin"
    fake_bin.mkdir()
    codex = fake_bin / "codex"
    codex.write_text("#!/bin/sh\nexit 0\n")
    codex.chmod(0o755)
    env["PATH"] = str(fake_bin) + os.pathsep + env.get("PATH", "")
    return env


def session_fixture(root, repo, count):
    root.mkdir()
    transcripts = root / "sessions"
    transcripts.mkdir()
    with sqlite3.connect(root / "state_5.sqlite") as database:
        database.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT, title TEXT, "
            "updated_at_ms INTEGER, source TEXT, archived INTEGER)"
        )
        database.executemany(
            "INSERT INTO threads VALUES (?, ?, ?, 2000, 'cli', 0)",
            (
                (
                    "selected-session" if index == 0 else f"db-{index}",
                    str(repo),
                    "Benchmark selection" if index == 0 else f"Synthetic {index}",
                )
                for index in range(count)
            ),
        )
    with (root / "history.jsonl").open("w") as history:
        for index in range(count):
            history.write(json.dumps({
                "session_id": str(uuid.UUID(int=index + 1)),
                "text": "Unmatched history",
                "ts": 2,
            }) + "\n")
            unmatched = uuid.UUID(int=1_000_000_000 + index)
            (transcripts / f"rollout-2026-09-07T10-00-00-{unmatched}.jsonl").write_text("{}\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/release/bonsai")
    parser.add_argument("--worktrees", nargs="*", type=int, default=[1, 10, 100])
    parser.add_argument("--sessions", nargs="*", type=int, default=[1000, 10000, 100000])
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--no-trace", action="store_true", help="skip the separate Git process-count pass")
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("this benchmark's synthetic provider executable requires a POSIX shell")
    if args.runs < 1 or args.timeout <= 0 or any(size < 1 for size in args.worktrees + args.sessions):
        parser.error("runs, timeout, and fixture sizes must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"build the release binary first: {binary}")
    git = shutil.which("git")
    if not git:
        parser.error("Git was not found on PATH")
    with tempfile.TemporaryDirectory(prefix="bonsai-benchmark-") as directory:
        root = Path(directory).resolve()
        repo = root / "repo"
        repo.mkdir()
        env = environment(root)
        run([git, "init", "-b", "ab/main"], repo, env, args.timeout)
        (repo / ".bonsai.toml").write_text(
            'workspace = false\ndefault_branch = "ab/main"\n'
            '[add]\nfetch = false\ninstall = false\ncopy = []\n'
        )
        run([git, "add", ".bonsai.toml"], repo, env, args.timeout)
        run([git, "commit", "-m", "chore: initialize benchmark fixture"], repo, env, args.timeout)
        branches = []
        try:
            for size in sorted(set(args.worktrees)):
                print(f"Preparing {size} managed worktrees", file=sys.stderr, flush=True)
                while len(branches) < size:
                    branch = f"ab/bench-{len(branches)}"
                    run([str(binary), "add", branch], repo, env, args.timeout)
                    branches.append(branch)
                for command in (["list", "--json"], ["list", "--status", "--json"], ["cd", branches[0]], ["clean", "--dry-run", "--no-fetch", "--json"]):
                    measure(binary, command, repo, env, args, "managed_worktrees", size)
        finally:
            if branches:
                run([str(binary), "remove", *branches], repo, env, args.timeout)
        for size in sorted(set(args.sessions)):
            print(f"Preparing {size} database sessions, history IDs, and unmatched rollouts", file=sys.stderr, flush=True)
            store = root / f"sessions-{size}"
            session_fixture(store, repo, size)
            session_env = dict(env, CODEX_HOME=str(store), CODEX_SQLITE_HOME=str(store))
            for query in ("selected-session", "Benchmark selection"):
                measure(binary, ["resume", query], repo, session_env, args, "sessions_per_source", size)


if __name__ == "__main__":
    main()
