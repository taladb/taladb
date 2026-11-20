# v0.13.2 — Final release readiness (M5.1)

| | |
|---|---|
| Date | 2026-11-20 |
| **Release candidate (source)** | **`dev` `1e8129cced2fb4e04da7560267472df46d2d9e8c`** = M2–M5 + #141 + **#142** (RN K1/K2/close) + **#143** (CHANGELOG + 0.13.2 bump) |
| Candidate `dev` push CI | run **44656529929**: **16/16 green** |
| Candidate Tier 2 (vs v0.13.1 + cumulative v0.12.1, release sizes) | run **44657268466**: **green**, 15 jobs + Tier 0 placeholder skipped; 1 pre-approved exception applied |
| PRs | #141, #142, #143 **merged by the maintainer** (00:15Z, 01:49:53Z, 01:50:34Z). **#142 and #143 were merged before their own CI finished**; it passed afterwards (K9). **#144** (CHANGELOG numbers + Known regressions, docs only): merged 02:53:08Z, `dev` head `a461176` |
| `dev` → `main` release PR | **not opened** |
| Tag / publish | not done |
| Baselines | v0.13.1 `236f5f1`, cumulative v0.12.1 `a2ef466` |

## Decision: **NO-GO**

The code side is done and green on the exact candidate. What blocks GO:

1. **No sign-off is recorded.** K1–K9 and S1-L1 each need a named approver (`V0132_FINAL_RELEASE_BLOCKERS.md` §2). **K7** (a Node process crash from a 10,000-level input, measured) is validated but not fixed, and needs an explicit accept-or-hold decision.
2. **K4 is pending.** The emulated arm64 test run had 16 binaries, 348 passed and 0 failed when last read, but had not finished.
3. **K3 was only partly executed.** Hermes host harness only; no iOS or Android run.
4. `dev` head `a461176` (#144) changed only `CHANGELOG.md`, and `dev` CI did not run on it. The tested source is `1e8129c`; `release.yml` re-runs the Tier 2 gate at tag time.

GO requires: the sign-offs, the K4 result, then a `dev` → `main` PR with every check green before merge. Tagging and publishing need separate approval.

## 1. PR #141 (B1): verified and merged

- **State:** #141 was merged by the maintainer at 2026-11-20 00:15:30Z (merge `ae525dc`). Its push CI (44644396867) is 16/16 green, including Tier 0 on x86_64 and arm64.
- **Review:** see `V0132_FINAL_RELEASE_BLOCKERS.md` §3. The header heuristic is sound for what it claims.
- **New M5.1 probes** (`m51-evidence/s1-probes/m51probe_mixed.rs`, output `probe-output.txt`), all on a fixture with 5 encrypted documents plus a plaintext version record:

| Case | Plain open on `ae525dc` | Passphrase open | Strict repair |
|---|---|---|---|
| A: plaintext doc in the same collection, newer ULID (sorts after) | **refused**, names 0.13.1 | refused, names 0.13.1 | refuses (mixed), writes nothing |
| B: plaintext doc in the same collection, old supplied ULID (sorts first) | **opens** | "not encrypted" | refuses (mixed), writes nothing |
| C: plaintext doc with a `deriveDocId` id | refused in **64/64** trials | — | — |
| D: damaged DB, zero documents (index only) | **opens** | "not encrypted" | repairs; then opens with passphrase |
| E: damaged DB, all documents deleted | **opens** | "not encrypted" | repairs; then opens with passphrase |
| F: B, then a public-API `insert` | insert **succeeds** (plaintext) | — | refuses, writes nothing |

The heuristic does **not** detect every mixed-format file. It catches the damage plus later time-ordered writes. B, D and E behave as in 0.13.1. These are documented in the CHANGELOG as limitations (S1-L1).

## 2. Documentation (B2): merged in #143 and #144

`CHANGELOG.md` `## 0.13.2 — Unreleased` covers:

- **Security:** S1 refusals, S2 128-level limit, S5, RN K1/K2.
- **Performance:** M2 #132/#133, with Linux CI ratios.
- **Fixes:** R1, R2, R3, R4, RN close, F1, F2.
- **Behaviour changes.**
- **The repair procedure:** prerequisites, a backup of the file *and* the salt sidecar, the original salt required, `encryption` feature, verify step. It is **Rust-only**: no Node, browser or React Native API. It cannot repair mixed files. `open_with_migrations` users must write their own version. Detection limits are listed.
- **Known limitations:** plaintext `export_snapshot` of encrypted databases, the R3 throughput trade-off with numbers, K7 Node nesting with measured numbers, `compact` no-op when encrypted, `aggregate` not decrypting field-encrypted values, the bare browser `aead::Error`.

**#144** (merged) replaces the M5-era performance ratios with the candidate's Tier 2 numbers and adds a "Known regressions" entry for the excepted arm64 case, as the 0.13.1 entry had.

**The recipe was compiled and run verbatim** (`m51-evidence/s1-probes/m51probe_recipe.rs`, `recipe-output.txt`). It repairs both fixture shapes (5/5 documents and the index readable afterwards), and refuses a wrong passphrase (`aead::Error`) and a mixed file (`format version 0x1a`) with every table byte-identical.

## 3. Versions (B3): done, merged in #143

| File | 0.13.1 → 0.13.2 |
|---|---|
| `Cargo.toml` `[workspace.package]` | ✓ |
| `Cargo.lock` (taladb, taladb-cli, taladb-ffi, taladb-node, taladb-web) | ✓ via `cargo update -w`; no third-party change |
| `packages/core/fuzz/Cargo.lock` (taladb, taladb-ffi) | ✓ |
| `package.json` (root) and `taladb`, `@taladb/web`, `@taladb/node`, `@taladb/react-native`, `@taladb/react` (+ peer `taladb ^0.13.2`) | ✓ via `sync-version.js` |
| `docs/.vitepress/config.mts` badge | ✓ |

The same 11 files as the 0.13.1 release commit `427b00f`. `sync-version.js --check 0.13.2` ✓ (the release workflow runs this against the tag). `TALADB_FFI_ABI_VERSION` 2 unchanged; `gen-header.sh --check` ✓. No storage-format or dependency change.

## 4. K1–K9

The full table is in `V0132_FINAL_RELEASE_BLOCKERS.md` §2. Key evidence:

### K1/K2 (fixed in #142), K3 (partly executed)

The real `packages/bindings/react-native/cpp/TalaDBHostObject.cpp` was built into a host harness. It runs inside **Hermes 0.14.1**, the macOS slice of the `hermes-ios` artifact that RN 0.83 uses (SHA-1 checked against Maven Central). It is linked against `libtaladb_ffi.a` from `ae525dc`, under ASan + UBSan. The harness's open/close functions repeat the steps of `TalaDBJni.cpp` and `TalaDB.mm`. Sources, build line and logs are in `m51-evidence/rn-hermes-harness/`.

| Suite | `dev` `ae525dc` | #142 |
|---|---|---|
| `k1`: keep `find`/`close`, drop the host, GC | **ASan heap-use-after-free** (host freed by HadesGC, then read by `find`) | 3/3 |
| `k2`: NaN/±Inf/−1/1e20/2e19 `topK` (findNearest, searchText), NaN/−5 `dims`, NaN/−1/2³²/1e300/Inf/"3" `setUserVersion`, 1e39 query value, plus in-range controls | UBSan float-cast-overflow at lines 518/585/680/736; invalid versions accepted; `"3"` aborts (JSI assert, exit 134) | 26/26 |
| `smoke`: open, insert/insertMany, find, findOne, count, updateOne, deleteOne, index, userVersion, callAsync + findAsync, `close()`, stale method after close, reopen (data, index, userVersion kept), `TalaDBModule.close()` + immediate reopen, encrypted insert, plain open of encrypted refused, wrong passphrase refused, correct passphrase recovers data, passphrase on plaintext refused, no salt invented, plaintext still opens | 15 pass, **3 fail**: the reopen after module close is locked ("Database already open"), still locked after a forced GC, and the later encryption steps fail on the same lock | 22/22 |
| `deep`: insert of 100 / 10,000 / 100,000-level objects | refused, no crash | refused, no crash |

**Not executed:** an iOS simulator, an Android emulator, a physical device, `TalaDB.mm` compilation, the Kotlin module and the RN bridge. Android: no SDK on this machine. iOS: Xcode 16.2, iOS 17.2–18.2 simulator runtimes, CocoaPods and the `x86_64-apple-ios` Rust target are present, but there is no RN app in the repository. Building one needs several GB of Pods/DerivedData, and 4.5 GB is free after cleanup. Recommended before tagging: create an RN 0.83 app, add `@taladb/react-native` from #142, `pod install`, and run the `smoke` sequence on an iPhone simulator.

### K4: arm64 functional tests (pending)

Container `8596dbb53085` (`rust:1.98-slim-bookworm`, linux/arm64 under qemu) runs `cargo test -p taladb --all-features` on a source copy whose `packages/` tree matches `ae525dc` exactly (`diff -rq`, excluding build output). It started at 00:15Z. When last read (about 02:50Z, 2.5 h in) it had reported **16 test binaries, 348 passed, 0 failed** and was running `encrypted_open_safety` (PBKDF2 under qemu is slow), out of about 69 binaries. **Not complete; not reported as passed.** The full log is saved to `m51-evidence/arm64-emulated.txt` when it exits. Note: the container's `ARM_EXIT` line reports `grep`'s exit status, not cargo's, so the verdict must be read from the `test result:` lines. This is emulation, not arm64 hardware.

### K6: why a rejected open changes bytes but not data

A rejected open never commits a TalaDB write transaction: the check runs in a read transaction, before any migration. The file still changes physically because redb itself, on every open and close (successful or rejected), rewrites its header (the commit slot and recovery flags) and its allocator state, and trims trailing free space. Every table and every raw key/value pair, read through an unencrypted `RedbBackend`, is identical before and after (fingerprint asserts in `encrypted_open_safety`, including #141's tests). The salt sidecar is not created or touched. The M5 control showed the same physical change after a successful open + close with no writes (165–535 bytes, `m5-evidence/probes/m5probe-bytes.cjs`). So "unchanged" holds for the database's contents, not its bytes. Avoiding the physical change would need a read-only pre-open, which is new behaviour.

### K7: Node input nesting (measured in M5.1)

On `@taladb/node` built from `39bd9ac` (same Node binding code as `dev`), Node 22.23.4, macOS x86_64, `collection.insert(obj)` with `obj` nested N levels deep:

- N = 100 to 6,000: `invalid filter: JSON nested deeper than 64 levels`.
- N = 10,000: **SIGSEGV (exit 139)**, with no JS exception.

The overflow is in napi-rs's `FromNapiValue for serde_json::Value` (feature `serde-json`). It runs before `check_depth`. A fix replaces the `serde_json::Value` parameters of every exported method with a depth-checked type, which also changes the generated `index.d.ts`. That is not a minimal patch, so none was made. The browser (`check_js_depth`), RN (Hermes `JSON.stringify` cap, verified above) and FFI (`serde_json` parse cap) are not affected.

### K8: Chrome/OPFS

No new run in M5.1; M5 evidence stands (real Chrome e2e 93/93, S1/R4 probe). CI covers browser phases 1, 2, 3 and 6. Gaps: an S1-damaged OPFS file, and the bare `aead::Error` message.

### K9: branch protection (inspected, not changed)

- `dev`: no protection.
- `main`: `required_pull_request_reviews.required_approving_review_count = 1`, `required_signatures = true`, `required_status_checks.checks = []`, `enforce_admins = false`, `allow_force_pushes = true`.

Proposed, **not applied** (a settings change needs your go-ahead):

```sh
# dev: require the CI aggregate check before merge
gh api -X PUT repos/taladb/taladb/branches/dev/protection --input - <<'JSON'
{"required_status_checks":{"strict":false,"checks":[{"context":"CI — all checks passed"}]},
 "enforce_admins":false,"required_pull_request_reviews":null,"restrictions":null}
JSON
# main: same check, keep 1 review, no force pushes
gh api -X PUT repos/taladb/taladb/branches/main/protection --input - <<'JSON'
{"required_status_checks":{"strict":false,"checks":[{"context":"CI — all checks passed"}]},
 "enforce_admins":false,"required_pull_request_reviews":{"required_approving_review_count":1},
 "restrictions":null,"allow_force_pushes":false}
JSON
```

The `performance.yml` jobs (`Criterion (ubuntu-latest, pr)`, `Criterion (ubuntu-24.04-arm, pr)`, `Vector (…, pr)`, `Browser worker + OPFS (pr)`) can be added the same way. Their PR runs skip when only docs change, so require them only on `main`. Note that PUT on `main` replaces the whole rule, so `required_signatures` must be re-enabled afterwards (`POST …/protection/required_signatures`).

## 5. Final validation on the candidate `1e8129c`

| Check | Result | Run |
|---|---|---|
| Rust workspace tests, fmt, clippy `-D warnings` | ✓ | CI 44656529929 |
| MSRV 1.90 | ✓ | CI 44656529929 |
| Browser WASM build; Browser worker + OPFS (pr + cumulative) | ✓ | CI 44656529929; Tier 2 44657268466 |
| Node build + `taladb` / `@taladb/react` builds and tests | ✓ | CI 44656529929 |
| FFI header (`gen-header.sh --check`), RN native glue compile + NDK link | ✓ | CI 44656529929 |
| Tier 0 fingerprints + MEM-2, x86_64 and arm64 | ✓ | CI 44656529929 |
| Criterion vs v0.13.1 (78 cases + 11 at 100k, both archs) | ✓ 0 regressions; fingerprints 0 differ | Tier 2 44657268466 |
| Criterion vs v0.12.1 cumulative | ✓ 0 regressions on x86_64; arm64 `doc/count_indexed_eq/100000` 1.055, **excepted** (pre-approved ≤ 1.08, #125, unchanged) | Tier 2 44657268466 |
| Vector A/B (none + binary, both archs, pr + cumulative) | ✓ | Tier 2 44657268466 |
| cargo audit, Miri (FFI pointer layer), fuzz smoke | ✓ | CI 44656529929 |
| Linux arm64 functional (emulated qemu, `ae525dc` tree; `packages/core` identical to `1e8129c` except the fuzz lockfile) | **pending**: 16 binaries / 348 pass / 0 fail so far | local container `8596dbb53085` |
| RN on device / simulator | **not executed** | — |
| Windows | **not executed** | — |

Thresholds were not changed, nothing was suppressed, and no earlier run was counted for this commit. The M2 gains hold on the candidate: `count_indexed_range` 0.050–0.086 and `aggregate_group` 0.45–0.72 vs v0.13.1.

## 6. Local state

- Worktrees:
  - `../taladb-m51-rn` (#142, merged; can be removed)
  - `../taladb-m51-release` (branch for the PR that adds these reports)
  - `../taladb-m5-s1` (merged #141; node_modules deleted)
  - `../taladb-m5-base` (v0.13.1)
- Removed at your request: `taladb-m5-target` (2.1 GB), `taladb-m51-ffi-target`, `taladb-m51-target`, `taladb-m5-s1/node_modules`. About 4.5 GB is free.
- Kept: the arm64 container `8596dbb53085`, its volume `m5-arm64-target`, and the source copy it mounts (an earlier session's scratchpad), until K4 is read.

Stopped here. I merged nothing: #141–#144 were merged by the maintainer. Nothing was tagged or published, and no branch-protection setting was changed.
