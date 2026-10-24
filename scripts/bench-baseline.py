#!/usr/bin/env python3
"""Write a version-controlled benchmark baseline directory.

    bench-baseline.py --version 0.12.1 --commit a2ef466... RESULT.json... [--evidence-asset NAME --evidence-sha256 HEX]

RESULT files are `scripts/vector-bench-ci.py` outputs (any workload). From each,
the side built from `--commit` is kept: per case, the per-run statistics and
their median. A side built from a different commit is accepted only when its
engine sources are identical to `--commit` (`packages/core/src`, the core
manifest and `Cargo.lock`), which is checked with git and recorded.

The directory gets `manifest.json` (source, toolchain, harness and environment
provenance), one compact reference file per result, and `SHA256SUMS` over every
file in it. Tier 0 references (`fingerprints-*.json`, `allocations-*.json`,
`codegen.json`) written by `scripts/bench-tier0.py --record` are kept and
checksummed too. Full raw evidence belongs in a release asset, not here.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import statistics
import subprocess
import sys

sys.dont_write_bytecode = True  # importing the gate script must not leave scripts/__pycache__
REPO = Path(__file__).resolve().parent.parent
ENGINE_PATHS = ["packages/core/src", "packages/core/Cargo.toml", "Cargo.lock"]


def git(*args):
    return subprocess.check_output(["git", *args], cwd=REPO, text=True).strip()


def sha256_bytes(data: bytes):
    return hashlib.sha256(data).hexdigest()


def same_engine(a, b):
    return subprocess.run(["git", "diff", "--quiet", a, b, "--", *ENGINE_PATHS], cwd=REPO).returncode == 0


def side_of(result, commit):
    for label, side in result["sides"].items():
        if side["commit"] == commit:
            return label, "built from the baseline commit"
    for label in ("baseline", "candidate"):
        other = result["sides"][label]["commit"]
        if same_engine(commit, other):
            return label, f"built from {other[:10]}, whose engine sources are identical to {commit[:10]}"
    raise SystemExit(f"neither side of this result was built from {commit} or identical engine sources")


def gate_module():
    """The gate's own metric extraction, so a reference holds exactly what it compares."""
    spec = importlib.util.spec_from_file_location("vector_bench_ci", REPO / "scripts/vector-bench-ci.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def per_run(result, label):
    index = 0 if label == "baseline" else 1
    runs = [pair[index] for pair in result["attempts"][0]["pairs"]]
    if result["workload"] == "criterion":
        values = {case: [r[case] for r in runs] for case in sorted(runs[0])}
        unit = "ns"
    else:
        gate = gate_module()
        workload = {"hnsw": gate.Hnsw, "filtered": gate.Filtered, "concurrency": gate.Concurrency}[result["workload"]](None)
        metrics = [workload.metrics(r) for r in runs]
        values = {name: [m[name][1] for m in metrics] for name in metrics[0]}
        unit = "ms, or as named"
    return {case: {"unit": unit, "runs": [round(v, 4) for v in vs], "median": round(statistics.median(vs), 4)}
            for case, vs in values.items()}


def compact(path: Path, commit):
    result = json.loads(path.read_text())
    label, provenance = side_of(result, commit)
    side = result["sides"][label]
    env = result["environment"]
    name = result["workload"]
    args = result["arguments"]
    if name == "hnsw":
        name += f"-{args['count']}-{args['quantization']}-{'sync' if args['batch_size'] is None else 'staged'}-{args['cache_bytes'] or 8388608}"
    elif name in ("filtered", "concurrency"):
        name += f"-{args['count']}"
    elif args.get("criterion_sizes"):
        name += f"-{args['criterion_sizes']}"
    profile = result["tolerances_profile"] if result["workload"] == "criterion" else env["platform_key"]
    return f"{name}-{profile}.json", {
        "schema": 1, "workload": result["workload"], "source_result": path.name, "side": label,
        "provenance": provenance, "commit": side["commit"], "binary_sha256": side["binary_sha256"],
        "harness_sha256": result["harness_sha256"], "protocol": result["protocol"],
        "environment": {k: env.get(k) for k in ("platform", "cpu_model", "cpus", "governor", "rustc", "runner",
                                                 "measured_runs_pinned_to", "started_at")},
        "pairs": len(result["attempts"][0]["pairs"]), "cases": per_run(result, label)}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("results", type=Path, nargs="*")
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--evidence-asset", help="release asset holding the full raw evidence")
    parser.add_argument("--evidence-sha256")
    args = parser.parse_args()
    commit = git("rev-parse", f"{args.commit}^{{commit}}")
    harness = ["packages/core/benches", "packages/core/examples", "scripts"]
    dirty = git("status", "--porcelain", "--", *harness)
    if dirty:
        # The manifest names the harness by commit; an uncommitted one has none.
        raise SystemExit(f"commit the harness first:\n{dirty}")
    out = args.out or REPO / f"benchmarks/baselines/v{args.version}"
    out.mkdir(parents=True, exist_ok=True)
    references = {}
    for path in args.results:
        name, data = compact(path, commit)
        (out / name).write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
        references[name] = {"workload": data["workload"], "source_result": path.name, "provenance": data["provenance"]}
    tag = f"v{args.version}"
    tag_kind = subprocess.run(["git", "cat-file", "-t", tag], cwd=REPO, capture_output=True, text=True).stdout.strip() or None
    manifest_path = out / "manifest.json"
    manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
    manifest |= {
        "schema": 1,
        "version": args.version,
        "source": {
            "commit": commit,
            "tag": tag,
            # `commit` is a lightweight tag; `tag` an annotated one. A lightweight
            # tag can move, so gates name the commit.
            "tag_object": "lightweight" if tag_kind == "commit" else tag_kind,
            "tag_points_at_commit": tag_kind is not None and git("rev-parse", f"{tag}^{{commit}}") == commit,
            "committed_at": git("show", "-s", "--format=%cI", commit),
            "subject": git("show", "-s", "--format=%s", commit),
            "cargo_lock_sha256": sha256_bytes(subprocess.check_output(["git", "show", f"{commit}:Cargo.lock"], cwd=REPO)),
            "core_tree": git("rev-parse", f"{commit}:packages/core"),
        },
        "toolchain": {
            "rust_toolchain_toml": subprocess.check_output(["git", "show", f"{commit}:rust-toolchain.toml"], cwd=REPO, text=True)
            .split("channel = ")[1].split("\n")[0].strip('"'),
            "msrv": git("show", f"{commit}:Cargo.toml").split("rust-version = ")[1].split("\n")[0].strip('"'),
        },
        "harness": {
            "commit": git("rev-parse", "HEAD"),
            "engine_identical_to_source": same_engine(commit, "HEAD"),
            "benches_tree": git("rev-parse", "HEAD:packages/core/benches"),
            "examples_tree": git("rev-parse", "HEAD:packages/core/examples"),
            "scripts": {p: git("rev-parse", f"HEAD:scripts/{p}") for p in ("vector-bench-ci.py", "bench-tier0.py",
                                                                         "vector-web-bench-compare.mjs", "bench-baseline.py")},
        },
    }
    manifest.setdefault("references", {}).update(references)
    if args.evidence_asset:
        manifest["evidence"] = {"release": tag, "asset": args.evidence_asset, "sha256": args.evidence_sha256}
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    sums = [f"{sha256_bytes(p.read_bytes())}  {p.name}" for p in sorted(out.iterdir())
            if p.is_file() and p.name != "SHA256SUMS"]
    (out / "SHA256SUMS").write_text("\n".join(sums) + "\n")
    print(f"wrote {out}: {len(references)} reference file(s), {len(sums)} checksummed")


if __name__ == "__main__":
    main()
