#!/usr/bin/env python3
"""Tier 0 benchmark gates: deterministic checks that need no baseline build.

    codegen       VEC-6 — each scoring kernel still vectorised, per target
    fingerprints  result fingerprints of every Criterion workload
    allocations   MEM-2 — heap allocation counts of `stabilization_memory`

Each command compares against a committed v0.12.1 reference
(`benchmarks/baselines/v0.12.1/`) and exits non-zero on a regression, or writes
that reference with `--record`. See `benchmarks/README.md`.

The codegen check is a gate on *vectorisation*, not a stand-in for speed: a
kernel that stays vectorised can still get slower, which the timed
`vector/score_one` cases catch. It catches what no x86 timing can — the
2026-09-20 fused loop stayed vectorised on x86_64 and compiled scalar on
aarch64 and wasm32. A kernel fails when it loses packed f32 multiply/add
arithmetic on a target; changes in the instruction count are recorded and
reported, not failed, because a better loop can legitimately need fewer.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
REFERENCE = REPO / "benchmarks/baselines/v0.12.1"

# Optimised dependencies, unoptimised `taladb`: redb and postcard dominate the
# fixture setup, and this keeps the run under a minute without a release build.
# Fingerprints and allocation counts are identical to the release build's.
DEV_DEPS_OPT = ["--profile", "dev", "--config", 'profile.dev.package."*".opt-level=2']

# Target -> (package to build, extra cargo args). wasm32 goes through taladb-web:
# only that crate selects getrandom's JS backend. `--no-default-features` drops
# the redb 2 migration reader, whose cdylib output needs a target linker.
TARGETS = {
    "x86_64-unknown-linux-gnu": ("taladb", ["--no-default-features"]),
    "aarch64-unknown-linux-gnu": ("taladb", ["--no-default-features"]),
    "aarch64-apple-ios": ("taladb", ["--no-default-features"]),
    "wasm32-unknown-unknown": ("taladb-web", []),
}

# The public, out-of-line scoring entry points. `score_with_query_norm` carries
# the inlined cosine reduction (`dot_and_norm_sq`). `norm_sq` is inlined into
# every caller and has no symbol of its own; it is not counted.
KERNELS = ("dot_similarity", "euclidean_similarity", "score_with_query_norm")

# What counts as a vector instruction on each target.
VECTOR_OP = {
    # Packed single/double SSE/AVX mnemonics (`mulps`, `vaddps`, `movups`, ...);
    # the scalar forms end in `ss`/`sd`.
    "x86_64": re.compile(r"^\s*v?[a-z0-9]+p[sd]\b"),
    # NEON operations on four f32 lanes.
    "aarch64": re.compile(r"\.4s\b"),
    "wasm32": re.compile(r"^\s*(f32x4|v128)\."),
}
# Packed f32 arithmetic: a kernel is vectorised on a target when its body
# multiplies and accumulates in vector registers. Multiplies only happen in the
# reduction loop, so a scalar loop with a vector horizontal sum does not pass.
PACKED_MUL = {
    "x86_64": re.compile(r"^\s*(v?mulps|vfn?m(add|sub)\w*ps)\b"),
    # GNU syntax `fmul v0.4s, ...`; Apple's (iOS) `fmul.4s v0, ...`.
    "aarch64": re.compile(r"^\s*(fmul|fmla|fmls)(\.4s\s|\s+v\d+\.4s)"),
    "wasm32": re.compile(r"^\s*f32x4\.(mul|relaxed_madd)\b"),
}
PACKED_ADD = {
    "x86_64": re.compile(r"^\s*(v?addps|vfn?m(add|sub)\w*ps)\b"),
    "aarch64": re.compile(r"^\s*(fadd|faddp|fmla)(\.4s\s|\s+v\d+\.4s)"),
    "wasm32": re.compile(r"^\s*f32x4\.(add|relaxed_madd)\b"),
}
# Every kernel is written to vectorise on every target (lane-split
# accumulators, see `LANES` in vector.rs), so the expectation is declared here
# rather than read from a reference that could be re-recorded from a bad build.
EXPECTED_VECTORISED = {(target, kernel) for target in TARGETS for kernel in KERNELS}
# A vectorised kernel whose instruction count falls further than this is noted.
COUNT_NOTE_DROP = 0.25
FUNCTION_END = re.compile(r"^\s*(\.cfi_endproc|end_function)\b")


def arch_key():
    return f"{platform.system().lower()}-{platform.machine()}"


def run(command, **kwargs):
    print("+", " ".join(command), file=sys.stderr)
    return subprocess.run(command, cwd=REPO, check=True, **kwargs)


def kernel_counts(asm: Path, family: str):
    counts = {}
    lines = asm.read_text().splitlines()
    for kernel in KERNELS:
        # v0 mangling: ...6taladb6vector<len><name>; Mach-O adds a leading `_`.
        label = re.compile(rf"^_{{1,2}}R\S*6taladb6vector{len(kernel)}{kernel}:$")
        starts = [i for i, line in enumerate(lines) if label.match(line)]
        if len(starts) != 1:
            counts[kernel] = None
            continue
        body = []
        for line in lines[starts[0] + 1:]:
            if FUNCTION_END.match(line):
                break
            body.append(line)
        mul = sum(bool(PACKED_MUL[family].search(line)) for line in body)
        add = sum(bool(PACKED_ADD[family].search(line)) for line in body)
        counts[kernel] = {"vector_instructions": sum(bool(VECTOR_OP[family].search(line)) for line in body),
                          "packed_mul": mul, "packed_add": add, "vectorised": mul > 0 and add > 0}
    return counts


def codegen(args):
    target_dir = Path(args.target_dir or tempfile.mkdtemp(prefix="taladb-codegen-"))
    results = {}
    for target, (package, extra) in TARGETS.items():
        family = target.split("-")[0]
        # `--config` joins this list with `.cargo/config.toml`'s rustflags for the
        # target (which enable simd128 on wasm32); RUSTFLAGS would replace them.
        # LTO off: with it on, cargo compiles the rlib as bitcode for the linker
        # and its assembly comes from LLVM's pre-link pipeline, which runs no
        # vectoriser — every native kernel then looks scalar. After LTO the
        # kernels are inlined and have no symbol to count. The non-LTO rlib is
        # the normal pipeline: what a crates.io user's release build compiles,
        # and the same vector instruction counts as a post-LTO build of the
        # kernels (checked on aarch64 through an exported wrapper, 2026-10-24).
        run(["cargo", "build", "--locked", "--release", "--lib", "-p", package, "--target", target, *extra,
             "--config", f'target.{target}.rustflags=["--emit=asm,link"]', "--config", "profile.release.lto=false"],
            env={**os.environ, "CARGO_TARGET_DIR": str(target_dir)})
        asm = sorted((target_dir / target / "release/deps").glob("taladb-*.s"), key=lambda p: p.stat().st_mtime)
        if not asm:
            raise SystemExit(f"no assembly for taladb on {target}")
        results[target] = kernel_counts(asm[-1], family)
    report = {"schema": 2, "rustc": rustc_version(),
              "rule": "fail when an expected kernel has no packed f32 multiply and add; instruction counts are recorded",
              "targets": results}
    if args.record:
        return write(args.reference, report)
    reference = json.loads(args.reference.read_text())
    failures, notes, rows, differences = [], [], [], []
    for target, kernels in results.items():
        for kernel, now in kernels.items():
            then = reference["targets"].get(target, {}).get(kernel)
            if isinstance(then, int):  # schema 1 reference: a bare count
                then = {"vector_instructions": then, "vectorised": None}
            if now is None:
                failures.append(f"{target} {kernel}: symbol not found (inlined away or renamed) — the kernel is no longer checked")
                rows.append((target, kernel, then and then["vector_instructions"], "missing", "", "missing"))
                continue
            before = then["vector_instructions"] if then else None
            delta = None if before is None else now["vector_instructions"] - before
            differences.append({"target": target, "kernel": kernel, "v0_12_1": before,
                                "candidate": now["vector_instructions"], "delta": delta})
            state = "vectorised" if now["vectorised"] else "SCALAR"
            rows.append((target, kernel, before, now["vector_instructions"],
                         "" if delta is None else f"{delta:+d}", f"{state} (mul {now['packed_mul']}, add {now['packed_add']})"))
            if (target, kernel) in EXPECTED_VECTORISED and not now["vectorised"]:
                failures.append(f"{target} {kernel}: no longer vectorised — {now['packed_mul']} packed f32 multiplies, "
                                f"{now['packed_add']} packed adds ({now['vector_instructions']} vector instructions, v0.12.1 had {before})")
            elif before and delta < 0 and -delta / before > COUNT_NOTE_DROP:
                notes.append(f"{target} {kernel}: still vectorised, but {now['vector_instructions']} vector instructions "
                             f"vs {before} at v0.12.1 ({delta / before:+.0%}); check `vector/score_one` timings")
    summary("VEC-6 vectorisation of the scoring kernels",
            ["Target", "Kernel", "v0.12.1 vector instr.", "Candidate", "Δ", "Candidate state"], rows,
            failures, notes)
    return finish(args.output, report | {"reference": reference["targets"], "differences": differences, "notes": notes},
                  failures)


def fingerprints(args):
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "fingerprints.json"
        env = {**os.environ, "TALADB_BENCH_FINGERPRINTS": str(out)}
        if args.sizes:
            env["TALADB_BENCH_SIZES"] = args.sizes
        run(["cargo", "bench", "--locked", "-p", "taladb", "--bench", "engine", *DEV_DEPS_OPT], env=env)
        cases = json.loads(out.read_text())["cases"]
    report = {"schema": 1, "platform": arch_key(), "sizes": args.sizes or "default", "cases": cases}
    if args.record:
        return write(args.reference, report)
    reference = json.loads(args.reference.read_text())["cases"]
    failures = compare_fingerprints(reference, cases)
    new = sorted(set(cases) - set(reference))
    rows = [(case, reference[case]["hash"], cases.get(case, {}).get("hash", "missing"))
            for case in sorted(reference) if cases.get(case) != reference[case]]
    summary(f"Result fingerprints: {len(cases)} cases, {len(rows)} differ from v0.12.1"
            + (f", {len(new)} new (not in the reference): {', '.join(new)}" if new else ""),
            ["Case", "v0.12.1", "Candidate"], rows, failures)
    return finish(args.output, report | {"new_cases": new}, failures)


def compare_fingerprints(reference, cases):
    failures = []
    for case, expected in sorted(reference.items()):
        actual = cases.get(case)
        if actual is None:
            failures.append(f"{case}: workload missing from the candidate harness")
        elif actual != expected:
            failures.append(f"{case}: {expected['kind']} n={expected['n']} {expected['hash']} -> "
                            f"{actual['kind']} n={actual['n']} {actual['hash']}")
    return failures


ALLOCATION_LINE = re.compile(r"^phase=(\w+) allocations=(\d+) allocated_bytes=(\d+)$")


def allocations(args):
    run(["cargo", "build", "--locked", "-p", "taladb", "--example", "stabilization_memory", *DEV_DEPS_OPT])
    target = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
    binary = target / "debug/examples/stabilization_memory"
    results = {}
    for operation in ("index", "update", "delete"):
        output = subprocess.run([str(binary), str(args.count), operation], check=True, capture_output=True, text=True).stdout
        phases = {m[1]: (int(m[2]), int(m[3])) for m in map(ALLOCATION_LINE.match, output.splitlines()) if m}
        # The workload's own allocations: from the loaded fixture to the end of
        # the mutation, and the export after it.
        results[operation] = {
            "mutation_allocations": phases["mutated"][0] - phases["loaded"][0],
            "mutation_bytes": phases["mutated"][1] - phases["loaded"][1],
            "export_allocations": phases["exported"][0] - phases["mutated"][0],
            "export_bytes": phases["exported"][1] - phases["mutated"][1],
        }
    report = {"schema": 1, "platform": arch_key(), "documents": args.count, "workloads": results}
    if args.record:
        return write(args.reference, report)
    reference = json.loads(args.reference.read_text())
    if reference["documents"] != args.count:
        raise SystemExit(f"reference was recorded at {reference['documents']} documents, not {args.count}")
    failures, rows = [], []
    for operation, metrics in results.items():
        for metric, value in metrics.items():
            expected = reference["workloads"][operation][metric]
            rows.append((operation, metric, expected, value))
            if value > expected * (1 + args.tolerance):
                failures.append(f"{operation} {metric}: {value} vs {expected} at v0.12.1 "
                                f"(+{value / expected - 1:.1%}, limit +{args.tolerance:.0%})")
    summary(f"MEM-2 allocations ({args.count} documents)", ["Workload", "Metric", "v0.12.1", "Candidate"], rows, failures)
    return finish(args.output, report | {"reference": reference["workloads"], "tolerance": args.tolerance}, failures)


def rustc_version():
    return subprocess.check_output(["rustc", "--version"], cwd=REPO, text=True).strip()


def write(path: Path, report):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"recorded {path}")


def summary(title, header, rows, failures, notes=()):
    lines = [title, "", "| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    lines += ["| " + " | ".join(str(c) for c in row) + " |" for row in rows]
    lines += ["", *(f"NOTE: {n}" for n in notes), *(f"FAIL: {f}" for f in failures)]
    text = "\n".join(lines) + "\n"
    print(text)
    if path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(path, "a") as file:
            file.write(text)


def finish(output, report, failures):
    if output:
        output.write_text(json.dumps(report | {"failures": failures}, indent=2) + "\n")
    if failures:
        raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("codegen", "fingerprints", "allocations"):
        p = sub.add_parser(name)
        p.add_argument("--record", action="store_true", help="write the reference instead of checking against it")
        p.add_argument("--output", type=Path, help="also write the result JSON here")
    sub.choices["codegen"].add_argument("--reference", type=Path, default=REFERENCE / "codegen.json")
    sub.choices["codegen"].add_argument("--target-dir", help="CARGO_TARGET_DIR for the asm builds (default: a temporary directory)")
    sub.choices["fingerprints"].add_argument("--reference", type=Path, default=REFERENCE / f"fingerprints-{arch_key()}.json")
    sub.choices["fingerprints"].add_argument("--sizes", help="TALADB_BENCH_SIZES for the run, e.g. 1000")
    sub.choices["allocations"].add_argument("--reference", type=Path, default=REFERENCE / f"allocations-{arch_key()}.json")
    sub.choices["allocations"].add_argument("--count", type=int, default=1000)
    sub.choices["allocations"].add_argument("--tolerance", type=float, default=0.02)
    args = parser.parse_args()
    {"codegen": codegen, "fingerprints": fingerprints, "allocations": allocations}[args.command](args)


if __name__ == "__main__":
    main()
