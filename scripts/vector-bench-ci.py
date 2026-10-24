#!/usr/bin/env python3
"""Compare identical workloads on two checkouts on the same runner.

    vector-bench-ci.py BASELINE CANDIDATE --workload hnsw|filtered|concurrency|criterion
    vector-bench-ci.py calibrate A_A_RESULT.json... --profile linux-x86_64

Workloads: `hnsw` (examples/hnsw_profile.rs), `filtered`
(examples/vector_filter_profile.rs), `concurrency` (examples/
vector_memory_profile.rs --steady, report only) and `criterion` (benches/engine.rs).
The rules are in benchmarks/README.md:

- The baseline runs the candidate's harness, built in its own target directory.
- Both sides are built first, then run interleaved: B C B C B C.
- A timing regression needs the median ratio over the tolerance, the absolute
  floor, and two thirds of the paired runs agreeing. One bad run cannot fail.
- A suspected regression is re-measured with more interleaved pairs and fails
  only if it fails again on all pairs combined. Every attempt is kept.
- Result fingerprints, recall and fixture signatures are hard rules: a mismatch
  fails whatever the timing says, and that case's timing is not compared.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
import tomllib

REPO = Path(__file__).resolve().parent.parent
TOLERANCES = REPO / "benchmarks/gates/criterion-tolerances.json"
EXCEPTIONS = REPO / "benchmarks/gates/exceptions.json"

# Criterion protocol. Changing it invalidates the calibrated tolerances:
# recalibrate in the same change.
CRITERION_PROTOCOL = {"warm_up_s": 1, "measurement_s": 3, "samples": 30, "sampling": "flat"}

# Per-request vector gates: relative tolerance and absolute floor (ms); a
# regression must exceed both. The tolerances are the historical ones, which the
# A/A runs support. The floors here are only fallbacks: calibrated floors, per
# rule, collection size and runner, live in the tolerance file (`vector`), because
# one fixed floor was both too high at 2k (0.25 ms against a 0.26 ms exact
# search, so the gate could not fire below +96%) and below the A/A noise at 10k.
VECTOR_RULES = {
    "build_ms": (0.30, 500.0),
    "exact_mean_ms": (0.30, 0.25),
    "ann_p50_ms": (0.50, 0.25),
    "filtered_p50_ms": (0.50, 0.25),
}
# Calibrated floors never go below these (timer and scheduler granularity).
MIN_FLOOR = {"build_ms": 50.0, "exact_mean_ms": 0.01, "ann_p50_ms": 0.01, "filtered_p50_ms": 0.01}
RECALL_ALLOWANCE = 0.03
# A cold-cache p50 is close to one sample per query: gate it only over this many
# interleaved pairs (the v0.12.1 release was blocked by a one-run cold case).
COLD_GATE_MIN_RUNS = 5


def log(message):
    print(message, file=sys.stderr, flush=True)


def sha256(path: Path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def tree_sha256(root: Path):
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        digest.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes() + b"\0")
    return digest.hexdigest()


def git(repo, *args):
    return subprocess.check_output(["git", *args], cwd=repo, text=True).strip()


def platform_key():
    return f"{platform.system().lower()}-{platform.machine()}"


def read_first(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def pinned():
    """Measured runs only (builds keep every core): `--pin 5` on a quiet local
    machine. CI runners cannot pin reliably; interleaving carries the load there."""
    cpus = PIN.get("cpus")
    return (lambda: os.sched_setaffinity(0, cpus)) if cpus else None


PIN = {}


def environment():
    cpu = None
    for line in (read_first("/proc/cpuinfo") or "").splitlines():
        if line.lower().startswith(("model name", "cpu model")):
            cpu = line.split(":", 1)[1].strip()
            break
    return {
        "platform": platform.platform(),
        "platform_key": platform_key(),
        "cpu_model": cpu,
        "cpus": os.cpu_count(),
        "affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
        "measured_runs_pinned_to": sorted(PIN["cpus"]) if PIN.get("cpus") else None,
        "governor": read_first("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "loadavg": read_first("/proc/loadavg"),
        "rustc": subprocess.check_output(["rustc", "-vV"], cwd=REPO, text=True).strip().splitlines(),
        "runner": {k: os.environ.get(k) for k in ("RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion",
                                                   "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_JOB")},
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }


class Side:
    def __init__(self, label, repo: Path, target_dir: Path):
        self.label, self.repo, self.target_dir = label, repo, target_dir
        manifest = tomllib.loads((repo / "packages/core/Cargo.toml").read_text())
        self.package = manifest["package"]["name"]
        self.crate = self.package.replace("-", "_")
        self.commit = git(repo, "rev-parse", "HEAD")
        self.executable = None

    def env(self, extra=None):
        return {**os.environ, "CARGO_TARGET_DIR": str(self.target_dir), **(extra or {})}

    def build(self, command):
        started = time.monotonic()
        result = subprocess.run(command + ["--message-format=json-render-diagnostics"], cwd=self.repo,
                                env=self.env(), capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(f"{self.label} build failed:\n{result.stderr}")
        executables = [m["executable"] for m in map(json.loads, result.stdout.splitlines())
                       if m.get("reason") == "compiler-artifact" and m.get("executable")]
        self.executable = Path(executables[-1])
        self.build_s = time.monotonic() - started
        log(f"{self.label}: built {self.executable} at {self.commit} in {self.build_s:.0f} s")

    def metadata(self):
        return {"commit": self.commit, "package": self.package, "executable": str(self.executable),
                "binary_sha256": sha256(self.executable), "build_s": round(self.build_s, 1),
                "cargo_lock_sha256": sha256(self.repo / "Cargo.lock")}


def localise(text, crate):
    # The baseline may predate the crate rename (`taladb_core`).
    return text if crate == "taladb" else text.replace("taladb::", f"{crate}::")


def prepare_example(side: Side, template: str):
    # The baseline runs the candidate's harness, not a potentially stale suite.
    harness = side.repo / "packages/core/examples/vector_ci_profile.rs"
    if harness.exists():
        raise RuntimeError(f"temporary harness already exists: {harness}")
    harness.write_text(localise(template, side.crate))
    try:
        side.build(["cargo", "build", "--locked", "--release", "-p", side.package, "--example", "vector_ci_profile"])
    finally:
        harness.unlink(missing_ok=True)


def prepare_criterion(side: Side, benches: Path):
    target = side.repo / "packages/core/benches"
    command = ["cargo", "bench", "--locked", "-p", side.package, "--bench", "engine", "--no-run"]
    if target.resolve() == benches.resolve():
        return side.build(command)
    # Swap the candidate's benches in for the build only; the checkout is left
    # as it was found.
    backup = side.repo / "packages/core/.benches-before-ci"
    if backup.exists():
        raise RuntimeError(f"stale benches backup: {backup}")
    target.rename(backup)
    try:
        shutil.copytree(benches, target)
        for path in target.rglob("*.rs"):
            path.write_text(localise(path.read_text(), side.crate))
        side.build(command)
    finally:
        shutil.rmtree(target, ignore_errors=True)
        backup.rename(target)


class ExampleWorkload:
    example = None

    def __init__(self, args):
        self.args = args

    def command(self):
        raise NotImplementedError

    def run(self, side, attempt, index, cases=None):
        result = subprocess.run([str(side.executable), *self.command()], cwd=side.repo, env=side.env(),
                                capture_output=True, text=True, preexec_fn=pinned())
        if result.returncode:
            raise RuntimeError(f"{side.label} run failed:\n{result.stderr}")
        path = self.args.evidence_dir / f"attempt{attempt}" / f"{side.label}-{index}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(result.stdout)
        return json.loads(result.stdout)


class Hnsw(ExampleWorkload):
    example = "hnsw_profile.rs"

    def command(self):
        a = self.args
        command = [str(a.count), "0.6", "--json", "--quantization", a.quantization, "--queries", str(a.queries)]
        if a.batch_size is not None:
            command += ["--batch-size", str(a.batch_size)]
        if a.cache_bytes is not None:
            command += ["--cache-bytes", str(a.cache_bytes)]
        return command

    def signature(self, report):
        # Recall is deterministic for fixed seeds and IDs.
        return [(r["ef_search"], r["recall_at_10"], r["distances"]) for r in report["ann"]]

    def metrics(self, report):
        rows = {"build_ms": ("build_ms", report["build_ms"]),
                "exact_mean_ms": ("exact_mean_ms", report["exact_mean_ms"])}
        if report.get("step_p95_ms") is not None:
            rows["build step p95_ms"] = ("report", report["step_p95_ms"])
        for r in report["ann"]:
            rows[f"ann ef={r['ef_search']} p50_ms"] = ("ann_p50_ms", r["p50_ms"])
            if "storage_reads" in r:
                rows[f"ann ef={r['ef_search']} storage_reads"] = ("report", r["storage_reads"])
        return rows

    def correctness(self, before, after):
        failures, notes = [], []
        for old, new in zip(before["ann"], after["ann"], strict=True):
            if new["recall_at_10"] + RECALL_ALLOWANCE < old["recall_at_10"]:
                failures.append(f"recall at ef={new['ef_search']} fell by more than 3 percentage points "
                                f"({old['recall_at_10']:.3f} -> {new['recall_at_10']:.3f})")
            elif new["recall_at_10"] != old["recall_at_10"]:
                notes.append(f"recall at ef={new['ef_search']} changed {old['recall_at_10']:.3f} -> "
                             f"{new['recall_at_10']:.3f}; list it in the PR description")
        return failures, notes


class Filtered(ExampleWorkload):
    example = "vector_filter_profile.rs"

    def command(self):
        return [str(self.args.count), "--queries", str(self.args.queries)]

    def signature(self, report):
        return (tuple(report[k] for k in ("count", "dimensions", "queries", "body_bytes")),
                [(r["mode"], r["warm"], r["filter"], r["fingerprint"], r["distances"]) for r in report["cases"]])

    def metrics(self, report):
        return {f"{r['mode']} {r['filter']} {'warm' if r['warm'] else 'cold'} p50_ms":
                ("filtered_p50_ms", r["p50_ms"], r["warm"]) for r in report["cases"]}

    def correctness(self, before, after):
        if self.signature(before) != self.signature(after):
            return ["filtered workload inputs, result fingerprints or distance counts changed"], []
        return [], []


class Concurrency(ExampleWorkload):
    """CONC-2, report only: multi-thread timings on shared 4-vCPU runners are
    too noisy to block on, but the paired ratios still show a scaling change."""
    example = "vector_memory_profile.rs"

    def command(self):
        return ["--cache-bytes", str(self.args.cache_bytes or 8 * 1024 * 1024), "--count", str(self.args.count),
                "--queries", "20", "--steady", str(self.args.steady), "--mode", self.args.mode]

    def signature(self, report):
        return [(c["concurrency"], c["requests"], c["memory_fallbacks"], c["recall_at_10"]) for c in report["cases"]]

    def metrics(self, report):
        rows = {}
        for c in report["cases"]:
            rows[f"{c['concurrency']} threads throughput_per_s"] = ("report", c["throughput_per_s"])
            if c.get("cpu_ms_per_request") is not None:
                rows[f"{c['concurrency']} threads cpu_ms_per_request"] = ("report", c["cpu_ms_per_request"])
                rows[f"{c['concurrency']} threads cpu_cores_busy"] = ("report", c["cpu_cores_busy"])
        return rows

    def correctness(self, before, after):
        return [], []


class CriterionWorkload:
    def __init__(self, args):
        self.args = args

    def env(self, home, cases=None):
        a = self.args
        env = {"CRITERION_HOME": str(home), "TALADB_BENCH_FLAT": "1",
               "TALADB_BENCH_SAMPLES": str(CRITERION_PROTOCOL["samples"])}
        groups = a.criterion_groups
        if cases is not None:
            groups = ",".join(sorted({c.split("/")[0] for c in cases}))
        if groups:
            env["TALADB_BENCH_GROUPS"] = groups
        if a.criterion_sizes:
            env["TALADB_BENCH_SIZES"] = a.criterion_sizes
        if a.criterion_large:
            env["TALADB_BENCH_LARGE"] = "1"
        return env

    def run(self, side, attempt, index, cases=None):
        home = self.args.evidence_dir / f"attempt{attempt}" / "criterion" / f"{side.label}-{index}"
        shutil.rmtree(home, ignore_errors=True)
        home.mkdir(parents=True)
        command = [str(side.executable), "--bench",
                   "--warm-up-time", str(CRITERION_PROTOCOL["warm_up_s"]),
                   "--measurement-time", str(CRITERION_PROTOCOL["measurement_s"]),
                   "--sample-size", str(CRITERION_PROTOCOL["samples"]), "--noplot"]
        pattern = self.args.criterion_filter
        if cases is not None:
            pattern = "^(" + "|".join(re.escape(c) for c in sorted(cases)) + ")$"
        if pattern:
            command.append(pattern)
        started = time.monotonic()
        with open(home / "stdout.txt", "w") as out:
            result = subprocess.run(command, cwd=side.repo, env=side.env(self.env(home, cases)),
                                    stdout=out, stderr=subprocess.STDOUT, preexec_fn=pinned())
        if result.returncode:
            raise RuntimeError(f"{side.label} criterion run failed; see {home / 'stdout.txt'}")
        log(f"{side.label} run {index}: {time.monotonic() - started:.0f} s")
        return read_criterion(home)

    def fingerprints(self, side):
        path = self.args.evidence_dir / f"fingerprints-{side.label}.json"
        env = self.env(self.args.evidence_dir / "criterion-unused")
        env["TALADB_BENCH_FINGERPRINTS"] = str(path)
        result = subprocess.run([str(side.executable)], cwd=side.repo, env=side.env(env), capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(f"{side.label} fingerprint run failed:\n{result.stderr}")
        return json.loads(path.read_text())["cases"]


def read_criterion(home: Path):
    """Per case: the median of the sample means, in nanoseconds."""
    medians = {}
    for bench in home.rglob("new/benchmark.json"):
        info = json.loads(bench.read_text())
        sample = json.loads((bench.parent / "sample.json").read_text())
        means = [t / i for t, i in zip(sample["times"], sample["iters"], strict=True)]
        medians[info["full_id"]] = statistics.median(means)
    return medians


def verdict(before, after, tolerance, floor):
    """The decision rule on paired per-run statistics (lists of equal length)."""
    b, c = statistics.median(before), statistics.median(after)
    ratios = [y / x for x, y in zip(before, after, strict=True)]
    needed = math.ceil(2 * len(ratios) / 3)
    slower = sum(r > 1 + tolerance / 2 for r in ratios)
    faster = sum(r < 1 / (1 + tolerance / 2) for r in ratios)
    if c / b > 1 + tolerance and c - b > floor and slower >= needed:
        result = "regression"
    elif b / c > 1 + tolerance and b - c > floor and faster >= needed:
        result = "improvement"
    else:
        result = "neutral"
    return {"baseline": b, "candidate": c, "ratio": c / b, "pair_ratios": ratios, "tolerance": tolerance,
            "floor": floor, "verdict": result, "pairs": len(ratios), "pairs_over_half_t": slower}


def load_tolerances(profile):
    data = json.loads(TOLERANCES.read_text())
    table = data["profiles"].get(profile)
    if table is None:
        log(f"no calibrated tolerances for {profile}; using the default for every case")
        table = {"cases": {}}
    return data["default"], table["cases"]


def vector_key(arguments, rule):
    """A floor belongs to one step configuration: an 8 MiB 10k graph build and a
    binary 64 MiB one differ by 7x in time and in absolute noise."""
    if arguments["workload"] == "filtered":
        return f"{rule}/{arguments['count']}"
    mode = "sync" if arguments.get("batch_size") is None else "staged"
    return f"{rule}/{arguments['count']}/{arguments['quantization']}/{mode}/{arguments.get('cache_bytes') or 8388608}"


def vector_floor(profile, key, rule):
    """Calibrated absolute floor for a per-request rule, or the historical one."""
    data = json.loads(TOLERANCES.read_text()).get("vector", {}).get("profiles", {}).get(profile, {})
    entry = data.get(key)
    return (entry["floor"], True) if entry else (VECTOR_RULES[rule][1], False)


def load_exceptions(gate_kind, profile):
    if not EXCEPTIONS.exists():
        return []
    entries = json.loads(EXCEPTIONS.read_text())["exceptions"]
    for entry in entries:
        missing = [k for k in ("case", "gate", "platform", "max_ratio", "reason", "approved_by", "approved_in")
                   if not entry.get(k)]
        if missing:
            raise SystemExit(f"benchmark exception {entry.get('case')!r} is missing {', '.join(missing)}")
    return [e for e in entries if e["gate"] in (gate_kind, "any") and e["platform"] in (profile, "*")]


def excepted(case, ratio, exceptions):
    """An exception covers a regression only up to the ratio it was approved for."""
    for entry in exceptions:
        if entry["case"] == case and ratio <= entry["max_ratio"]:
            return entry
    return None


def judge(v, message, gated, exceptions, failures, warnings, applied, why_ungated):
    if v["verdict"] != "regression":
        return
    if not gated:
        warnings.append(f"report-only {message} ({why_ungated})")
    elif entry := excepted(v["case"], v["ratio"], exceptions):
        applied.append(entry | {"measured_ratio": v["ratio"]})
        warnings.append(f"excepted {message} — {entry['reason']} (approved by {entry['approved_by']} in {entry['approved_in']})")
    else:
        failures.append(message)


def evaluate(workload, pairs, args, exceptions):
    """`pairs`: [(baseline_result, candidate_result), ...] across attempts."""
    rows, failures, warnings, applied = [], [], [], []
    if isinstance(workload, CriterionWorkload):
        default, table = load_tolerances(args.tolerances_profile)
        for case in sorted(set.intersection(*(set(b) & set(c) for b, c in pairs))):
            entry = table.get(case, {})
            tolerance = entry.get("T", default["T"])
            gated = entry.get("gate", default["gate"]) and case not in args.excluded_cases
            before = [b[case] for b, _ in pairs]
            v = verdict(before, [c[case] for _, c in pairs], tolerance, max(2000.0, 0.02 * statistics.median(before)))
            v |= {"case": case, "gated": gated, "calibrated": case in table}
            rows.append(v)
            message = (f"{case}: {v['baseline'] / 1e3:.2f} -> {v['candidate'] / 1e3:.2f} µs "
                       f"({v['ratio'] - 1:+.1%}, T {tolerance:.0%}, {v['pairs_over_half_t']}/{v['pairs']} pairs over T/2)")
            judge(v, message, gated, exceptions, failures, warnings, applied, "too noisy to gate on this runner")
        return rows, failures, warnings, applied

    for label, reports in (("baseline", [b for b, _ in pairs]), ("candidate", [c for _, c in pairs])):
        if any(workload.signature(r) != workload.signature(reports[0]) for r in reports):
            failures.append(f"{label}: workload results changed between repeated runs of the same binary")
    hard, notes = workload.correctness(pairs[0][0], pairs[0][1])
    failures += hard
    warnings += notes
    if failures and isinstance(workload, Filtered):
        return rows, failures, warnings, applied
    first = workload.metrics(pairs[0][0])
    for name, spec in first.items():
        before = [workload.metrics(b)[name][1] for b, _ in pairs]
        after = [workload.metrics(c)[name][1] for _, c in pairs]
        if spec[0] == "report":
            ratios = [y / x if x else float("nan") for x, y in zip(before, after, strict=True)]
            b, c = statistics.median(before), statistics.median(after)
            rows.append({"case": name, "baseline": b, "candidate": c, "ratio": c / b if b else float("nan"),
                         "pair_ratios": ratios, "verdict": "report", "gated": False, "pairs": len(ratios)})
            continue
        tolerance = VECTOR_RULES[spec[0]][0]
        floor, calibrated = vector_floor(args.tolerances_profile, vector_key(vars(args), spec[0]), spec[0])
        cold = len(spec) > 2 and not spec[2]
        gated = not (cold and len(pairs) < COLD_GATE_MIN_RUNS) and name not in args.excluded_cases
        v = verdict(before, after, tolerance, floor) | {"case": name, "gated": gated, "calibrated": calibrated}
        rows.append(v)
        message = (f"{name}: {v['baseline']:.3f} -> {v['candidate']:.3f} ({v['ratio'] - 1:+.1%}, "
                   f"limit +{tolerance:.0%} and {floor:g} ms, {v['pairs_over_half_t']}/{v['pairs']} pairs over T/2)")
        judge(v, message, gated, exceptions, failures, warnings, applied,
              f"cold cache, {len(pairs)} pairs < {COLD_GATE_MIN_RUNS}")
    return rows, failures, warnings, applied


def measure(workload, sides, attempt, repeats, cases=None):
    pairs = []
    for index in range(1, repeats + 1):
        results = []
        for side in sides:
            log(f"attempt {attempt}, pair {index}/{repeats}: {side.label}")
            results.append(workload.run(side, attempt, index, cases))
        pairs.append(tuple(results))
    return pairs


def summary_lines(args, rows, failures, warnings, attempts, sides):
    title = {"criterion": f"Criterion engine benchmarks ({args.tolerances_profile} tolerances)",
             "hnsw": f"HNSW: {args.count:,} vectors, {args.quantization} quantization, "
                     f"{'sync' if args.batch_size is None else f'batch size {args.batch_size}'}, "
                     f"cache {args.cache_bytes or 8388608} bytes",
             "filtered": f"Filtered vector search: {args.count:,} vectors",
             "concurrency": f"Concurrent {args.mode} search (report only): {args.count:,} vectors, "
                            f"{args.steady} requests per thread"}[args.workload]
    lines = [f"### {title} — {args.gate_kind} comparison",
             f"Baseline `{sides[0].commit[:10]}`, candidate `{sides[1].commit[:10]}`; interleaved pairs per attempt: "
             f"{', '.join(str(len(a['pairs'])) for a in attempts)}",
             "", "| Case | Baseline | Candidate | Ratio | Paired ratios | T | Verdict |",
             "|---|---:|---:|---:|---|---:|---|"]
    scale, unit = (1e3, " µs") if args.workload == "criterion" else (1.0, "")
    for r in rows:
        if args.only_changed and r["verdict"] in ("neutral", "report"):
            continue
        tolerance = f"{r['tolerance']:.0%}" if "tolerance" in r else ""
        state = r["verdict"] if r.get("gated", True) or r["verdict"] == "report" else f"{r['verdict']} (report-only)"
        if r.get("confirmed"):
            state += ", confirmation"
        lines.append(f"| {r['case']} | {r['baseline'] / scale:.3f}{unit} | {r['candidate'] / scale:.3f}{unit} | "
                     f"{r['ratio']:.3f} | {' '.join(f'{x:.2f}' for x in r['pair_ratios'])} | {tolerance} | {state} |")
    if args.workload == "criterion":
        fp = args.fingerprint_summary
        lines += ["", f"Result fingerprints: {fp['cases']} cases compared, {len(fp['mismatched'])} differ"]
    lines += ["", *(f"NOTE: {w}" for w in warnings), *(f"FAIL: {f}" for f in failures)]
    return lines


def compare(args):
    kinds = {"hnsw": Hnsw, "filtered": Filtered, "concurrency": Concurrency, "criterion": CriterionWorkload}
    workload = kinds[args.workload](args)
    args.evidence_dir.mkdir(parents=True, exist_ok=True)
    # One target directory per checkout path. Cargo's artifact names do not
    # depend on where a workspace lives, and its freshness check compares the
    # mtimes of the sources it last built from: a directory reused by another
    # checkout can report "fresh" and hand back the previous checkout's binary.
    target_root = Path(os.environ.get("CARGO_TARGET_DIR", args.candidate.resolve() / "target")).resolve()
    def target(label, repo):
        return target_root / f"vector-{label}-{hashlib.sha256(str(repo).encode()).hexdigest()[:8]}"
    sides = [Side(label, repo.resolve(), target(label, repo.resolve()))
             for label, repo in (("baseline", args.baseline), ("candidate", args.candidate))]
    if isinstance(workload, CriterionWorkload):
        benches = args.candidate.resolve() / "packages/core/benches"
        harness = tree_sha256(benches)
        for side in sides:
            prepare_criterion(side, benches)
    else:
        template = (args.candidate / "packages/core/examples" / workload.example).read_text()
        harness = hashlib.sha256(template.encode()).hexdigest()
        for side in sides:
            prepare_example(side, template)
    exceptions = load_exceptions(args.gate_kind, args.tolerances_profile)
    report = {"schema": 2, "workload": args.workload, "gate_kind": args.gate_kind,
              "tolerances_profile": args.tolerances_profile, "harness_sha256": harness,
              "arguments": {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
              "protocol": CRITERION_PROTOCOL if args.workload == "criterion"
              else {"vector_rules": VECTOR_RULES, "recall_allowance": RECALL_ALLOWANCE, "cold_gate_min_runs": COLD_GATE_MIN_RUNS},
              "environment": environment(), "sides": {s.label: s.metadata() for s in sides}}

    hard = []
    args.excluded_cases = set()
    if isinstance(workload, CriterionWorkload):
        before, after = (workload.fingerprints(s) for s in sides)
        mismatched = sorted(c for c in set(before) | set(after) if before.get(c) != after.get(c))
        report["fingerprints"] = {"baseline": before, "candidate": after, "mismatched": mismatched}
        args.excluded_cases = set(mismatched)
        args.fingerprint_summary = {"cases": len(set(before) | set(after)), "mismatched": mismatched}
        hard = [f"{c}: result fingerprint {before.get(c)} -> {after.get(c)}; timing not compared" for c in mismatched]

    attempts = [{"attempt": 1, "pairs": measure(workload, sides, 1, args.repeats)}]
    rows, failures, warnings, applied = evaluate(workload, attempts[0]["pairs"], args, exceptions)
    first = {"cases": rows, "failures": failures}
    suspects = [r["case"] for r in rows if r["verdict"] == "regression" and r.get("gated")
                and not excepted(r["case"], r["ratio"], exceptions)]
    unstable = any("changed between repeated runs" in f for f in failures)
    if suspects and args.confirm_repeats > 0 and not unstable:
        log(f"confirming {len(suspects)} suspected regression(s) with {args.confirm_repeats} more pairs: {', '.join(suspects)}")
        only = set(suspects) if args.workload == "criterion" else None
        confirm = measure(workload, sides, 2, args.confirm_repeats, only)
        attempts.append({"attempt": 2, "cases": sorted(suspects), "pairs": confirm})
        if only is not None:
            combined = [({k: v for k, v in b.items() if k in only}, {k: v for k, v in c.items() if k in only})
                        for b, c in attempts[0]["pairs"]] + confirm
            c_rows, c_failures, c_warnings, c_applied = evaluate(workload, combined, args, exceptions)
            merged = {r["case"]: r for r in rows} | {r["case"]: r | {"confirmed": True} for r in c_rows}
            rows = [merged[k] for k in sorted(merged)]
            failures = [f for f in failures if not any(f.startswith(c + ":") for c in suspects)] + c_failures
            warnings += c_warnings
            applied += c_applied
        else:
            rows, failures, warnings, applied = evaluate(workload, attempts[0]["pairs"] + confirm, args, exceptions)
            rows = [r | {"confirmed": True} for r in rows]
    failures = hard + failures
    report |= {"attempts": [{"attempt": a["attempt"], "cases": a.get("cases"), "pairs": [list(p) for p in a["pairs"]]}
                            for a in attempts],
               "first_attempt": first, "cases": rows, "warnings": warnings, "exceptions_applied": applied,
               "failures": failures, "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    report["arguments"]["excluded_cases"] = sorted(args.excluded_cases)
    report["environment"]["loadavg_after"] = read_first("/proc/loadavg")
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    text = "\n".join(summary_lines(args, rows, failures, warnings, attempts, sides)) + "\n"
    print(text)
    if path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(path, "a") as file:
            file.write(text)
    if failures:
        raise SystemExit(1)


def calibrate_vector(args):
    """Absolute floors for the per-request rules from A/A results: for each rule
    and collection size, 3 x the largest |median(C) - median(B)| over every A/A
    case of that rule. The relative tolerances stay as they are; the calibration
    records whether 3 x the largest A/A ratio deviation stays inside them."""
    worst, ratio, sources = {}, {}, []
    for path in args.results:
        result = json.loads(path.read_text())
        sides = result["sides"]
        if result["workload"] not in ("hnsw", "filtered"):
            continue
        if sides["baseline"]["commit"] != sides["candidate"]["commit"]:
            raise SystemExit(f"{path}: not an A/A run")
        workload = {"hnsw": Hnsw, "filtered": Filtered}[result["workload"]](None)
        pairs = result["attempts"][0]["pairs"]
        count = result["arguments"]["count"]
        sources.append({"file": str(path.relative_to(path.parents[1])), "workload": result["workload"], "count": count,
                        "pairs": len(pairs), "runner": result["environment"]["runner"]})
        arguments = result["arguments"]
        for name, spec in workload.metrics(pairs[0][0]).items():
            if spec[0] == "report":
                continue
            b = statistics.median(workload.metrics(p[0])[name][1] for p in pairs)
            c = statistics.median(workload.metrics(p[1])[name][1] for p in pairs)
            key = vector_key(arguments, spec[0])
            worst[key] = max(worst.get(key, 0.0), abs(c - b))
            ratio[key] = max(ratio.get(key, 0.0), abs(c / b - 1))
    data = json.loads(args.output.read_text())
    table = {}
    for key in sorted(worst):
        rule = key.split("/")[0]
        table[key] = {"floor": round(max(MIN_FLOOR[rule], 3 * worst[key]), 3), "aa_max_abs_diff": round(worst[key], 4),
                      "aa_max_ratio_dev": round(ratio[key], 4),
                      "relative_tolerance_holds": 3 * ratio[key] <= VECTOR_RULES[rule][0]}
    vector = data.setdefault("vector", {"profiles": {}, "calibration": {}})
    vector["profiles"][args.profile] = table
    vector["calibration"][args.profile] = {
        "rule": "floor = max(minimum, 3 x largest A/A |median(C) - median(B)|) per rule and step configuration; relative tolerances unchanged",
        "minimum_floor_ms": MIN_FLOOR, "relative_tolerances": {k: v[0] for k, v in VECTOR_RULES.items()},
        "sources": sources, "calibrated_at": time.strftime("%Y-%m-%d", time.gmtime())}
    args.output.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    for key, entry in table.items():
        print(f"{args.profile} {key}: floor {entry['floor']} ms (A/A max diff {entry['aa_max_abs_diff']}, "
              f"ratio dev {entry['aa_max_ratio_dev']:.1%}{'' if entry['relative_tolerance_holds'] else ', 3x exceeds the relative tolerance'})")


def reevaluate(args):
    """Judge a stored result again under the current tolerance tables, from its
    recorded runs (nothing is re-measured). Prints the verdicts that changed."""
    result = json.loads(args.result.read_text())
    stored = result["arguments"]
    ns = argparse.Namespace(**{**stored, "tolerances_profile": args.tolerances_profile or result["tolerances_profile"],
                               "excluded_cases": set(stored.get("excluded_cases", [])),
                               "evidence_dir": Path("."), "output": Path(".")})
    kinds = {"hnsw": Hnsw, "filtered": Filtered, "concurrency": Concurrency, "criterion": CriterionWorkload}
    workload = kinds[result["workload"]](ns)
    exceptions = load_exceptions(result["gate_kind"], ns.tolerances_profile)
    attempts = [[tuple(p) for p in a["pairs"]] for a in result["attempts"]]
    rows, failures, warnings, _ = evaluate(workload, attempts[0], ns, exceptions)
    first = list(failures)
    suspects = [r["case"] for r in rows if r["verdict"] == "regression" and r.get("gated")]
    confirmed = None
    if suspects:
        if len(attempts) > 1:
            if result["workload"] == "criterion":
                only = set(suspects) & set(attempts[1][0][0])
                combined = [({k: v for k, v in b.items() if k in only}, {k: v for k, v in c.items() if k in only})
                            for b, c in attempts[0]] + attempts[1]
                _, confirmed, _, _ = evaluate(workload, combined, ns, exceptions)
                missing = sorted(set(suspects) - only)
                confirmed += [f"{c}: suspected now, never confirmed when measured" for c in missing]
            else:
                _, confirmed, _, _ = evaluate(workload, attempts[0] + attempts[1], ns, exceptions)
    final = confirmed if confirmed is not None else first
    report = {"result": str(args.result), "workload": result["workload"], "profile": ns.tolerances_profile,
              "stored_failures": result["failures"], "first_attempt_failures": first,
              "confirmation_available": len(attempts) > 1, "final_failures": final,
              "unconfirmed": bool(first) and confirmed is None}
    print(json.dumps(report, indent=1))
    return report


def calibrate(args):
    """Tolerances from A/A results (the same commit on both sides). N is the
    largest deviation of one run's median from the median of its side, over
    every side of every result; T = max(T_min, 3N)."""
    deviation, ratios, sources = {}, {}, []
    for path in args.results:
        result = json.loads(path.read_text())
        if result["workload"] != "criterion":
            raise SystemExit(f"{path}: not a criterion result")
        sides = result["sides"]
        if sides["baseline"]["commit"] != sides["candidate"]["commit"]:
            raise SystemExit(f"{path}: not an A/A run ({sides['baseline']['commit']} vs {sides['candidate']['commit']})")
        pairs = result["attempts"][0]["pairs"]
        sources.append({"file": path.name, "commit": sides["baseline"]["commit"], "pairs": len(pairs),
                        "runner": result["environment"]["runner"], "cpu_model": result["environment"]["cpu_model"]})
        for case in pairs[0][0]:
            for side in (0, 1):
                values = [p[side][case] for p in pairs]
                m = statistics.median(values)
                deviation[case] = max(deviation.get(case, 0.0), max(abs(v - m) / m for v in values))
            ratios.setdefault(case, []).append(statistics.median(p[1][case] for p in pairs)
                                               / statistics.median(p[0][case] for p in pairs))
    cases = {}
    for case in sorted(deviation):
        n = deviation[case]
        t = max(args.t_min, 3 * n)
        small = case.endswith("/1000")
        cases[case] = {"N": round(n, 4), "T": round(t, 3),
                       "gate": t <= args.t_cap and not (small and t > args.small_cap),
                       "aa_ratios": [round(r, 4) for r in ratios[case]]}
    data = json.loads(args.output.read_text()) if args.output.exists() else {"schema": 1, "profiles": {}}
    data.setdefault("default", {"T": 0.10, "gate": True})
    data["profiles"][args.profile] = {"cases": cases}
    data.setdefault("calibration", {})[args.profile] = {
        "rule": f"T = max({args.t_min:.0%}, 3N); report-only when T > {args.t_cap:.0%}, or for 1k cases when T > {args.small_cap:.0%}",
        "protocol": CRITERION_PROTOCOL, "sources": sources, "calibrated_at": time.strftime("%Y-%m-%d", time.gmtime())}
    args.output.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    gated = sum(c["gate"] for c in cases.values())
    print(f"{args.profile}: {len(cases)} cases from {len(sources)} A/A result(s), {gated} gated; "
          f"N median {statistics.median(c['N'] for c in cases.values()):.1%}, max {max(c['N'] for c in cases.values()):.1%}")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "calibrate":
        parser = argparse.ArgumentParser(prog="vector-bench-ci.py calibrate")
        parser.add_argument("results", type=Path, nargs="+")
        parser.add_argument("--profile", required=True, help="tolerance profile to write, e.g. ci-linux-x86_64")
        parser.add_argument("--vector", action="store_true", help="calibrate the per-request vector floors instead")
        parser.add_argument("--output", type=Path, default=TOLERANCES)
        parser.add_argument("--t-min", type=float, default=0.05)
        parser.add_argument("--t-cap", type=float, default=0.25)
        parser.add_argument("--small-cap", type=float, default=0.10)
        args = parser.parse_args(sys.argv[2:])
        return calibrate_vector(args) if args.vector else calibrate(args)
    if len(sys.argv) > 1 and sys.argv[1] == "reevaluate":
        parser = argparse.ArgumentParser(prog="vector-bench-ci.py reevaluate")
        parser.add_argument("result", type=Path)
        parser.add_argument("--tolerances-profile")
        report = reevaluate(parser.parse_args(sys.argv[2:]))
        raise SystemExit(1 if report["final_failures"] else 0)
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--workload", choices=("hnsw", "filtered", "concurrency", "criterion"), default="hnsw")
    parser.add_argument("--count", type=int, default=2000)
    parser.add_argument("--repeats", type=int, default=3, help="interleaved baseline/candidate pairs")
    parser.add_argument("--confirm-repeats", type=int, default=3, help="extra pairs for a suspected regression (0 = off)")
    parser.add_argument("--quantization", choices=("none", "scalar", "binary"), default="none")
    parser.add_argument("--queries", type=int, default=100)
    parser.add_argument("--batch-size", type=int, help="use resumable builds with 1..1024 vectors per step")
    parser.add_argument("--cache-bytes", type=int, help="vector cache budget (hnsw: the harness default is 8 MiB)")
    parser.add_argument("--steady", type=int, default=400, help="concurrency: requests per thread")
    parser.add_argument("--mode", choices=("ann", "exact"), default="ann", help="concurrency: search mode")
    parser.add_argument("--criterion-filter", help="Criterion regex over case ids")
    parser.add_argument("--criterion-groups", help="TALADB_BENCH_GROUPS, e.g. doc,stabilization")
    parser.add_argument("--criterion-sizes", help="TALADB_BENCH_SIZES, e.g. 1000,10000")
    parser.add_argument("--criterion-large", action="store_true", help="TALADB_BENCH_LARGE (adds 100k)")
    parser.add_argument("--tolerances-profile", default=platform_key())
    parser.add_argument("--gate-kind", choices=("pr", "cumulative"), default="pr",
                        help="which approved exceptions apply: against the PR base, or against v0.12.1")
    parser.add_argument("--only-changed", action="store_true", help="summary lists only non-neutral cases")
    parser.add_argument("--pin", help="CPU list for the measured runs, e.g. 5 or 2-5 (Linux)")
    parser.add_argument("--output", type=Path, default=Path("vector-benchmark.json"))
    parser.add_argument("--evidence-dir", type=Path, help="raw per-run output (default: <output>-evidence/)")
    args = parser.parse_args()
    if args.count < 10 or args.repeats < 1 or args.queries < 1 or args.confirm_repeats < 0:
        parser.error("count must be >= 10; repeats and queries must be positive")
    if args.batch_size is not None and not 1 <= args.batch_size <= 1024:
        parser.error("batch size must be between 1 and 1024")
    if args.workload == "filtered" and (args.batch_size is not None or args.quantization != "none" or args.cache_bytes):
        parser.error("filtered workloads use a fixed binary graph and their own cache budgets")
    if args.evidence_dir is None:
        args.evidence_dir = args.output.with_name(args.output.stem + "-evidence")
    # Absolute: the harnesses run with the checkout under test as their working
    # directory, and write fingerprints and Criterion output into this path.
    args.output, args.evidence_dir = args.output.resolve(), args.evidence_dir.resolve()
    if args.pin:
        PIN["cpus"] = {c for part in args.pin.split(",") for c in
                       (range(int(part.split("-")[0]), int(part.split("-")[-1]) + 1))}
    compare(args)


if __name__ == "__main__":
    main()
