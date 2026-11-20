# v0.13.2 — Final release checklist (M5.1)

`[x]` = verified · `[ ]` = still to do · `[~]` = needs a named sign-off · `[p]` = pending result

Release candidate: source `dev` **`1e8129c`** (#141, #142, #143); `dev` head **`a461176`** adds #144 (CHANGELOG only). All merged by the maintainer.

## Code and merge

- [x] M2 #132–#133, M3 #134–#139, M4 #140 merged into `dev`
- [x] PR #141 merged (00:15Z, by the maintainer); `dev` push CI 44644396867 on `ae525dc`: 16/16 green
- [x] #141 reviewed in M5.1; detection limits probed (A/C caught; B/D/E not) → S1-L1
- [x] K1 + K2 + RN close fix written, reproduced on `dev`, passing in the Hermes harness → **PR #142**
- [x] #142 merged (01:49:53Z): **before its CI finished**; PR CI 44655653021 16/16 green afterwards
- [x] #143 merged (01:50:34Z): **before its CI finished**; CI 44655912015 + Vector 44655912087 green afterwards (24 pass, 1 skip by design)
- [x] `dev` push CI on `1e8129c`: run 44656529929, 16/16 green
- [x] Tier 2 dispatch on `1e8129c`: run 44657268466 green (1 pre-approved exception applied: arm64 cumulative `count_indexed_eq/100000` 1.055 ≤ 1.08)
- [x] #144 (CHANGELOG numbers + Known regressions) merged 02:53:08Z (`a461176`); docs-only, so `dev` CI did not run on that SHA

## Documentation (B2), in #143

- [x] `## 0.13.2` entry: M2 perf, M3 fixes, M4/M5 security, behaviour changes
- [x] S1 repair recipe (Rust-only; prerequisites, file + salt backup, original salt, limits, `open_with_migrations` note) — **compiled and run verbatim**
- [x] Stated that no Node, browser or React Native API can repair, and mixed files cannot be repaired
- [x] `export_snapshot` of an encrypted DB is plaintext
- [x] 128-level `Value` limit; R3 throughput trade-off with numbers
- [x] Known limitations: K7 (Node deep input, measured), `compact` no-op encrypted, `aggregate` no field decryption, browser `aead::Error`, S1-L1 detection limits
- [ ] Set the heading date at tag time ("Unreleased" now)

## Mechanics (B3), in #143

- [x] 0.13.1 → 0.13.2 via `sync-version.js` + fuzz `Cargo.lock` (11 files, same set as `427b00f`)
- [x] `sync-version.js --check 0.13.2` ✓; `Cargo.lock` only workspace crates changed
- [x] `gen-header.sh --check` ✓; ABI stays 2; no storage-format or dependency change
- [ ] Release PR `dev` → `main` (only after everything above; all checks green before merge)
- [ ] Tag `v0.13.2` (**separate approval**; not done)
- [ ] Publish crates / npm (**separate approval**; not done)

## Validation on the candidate `1e8129c`

- [x] Rust workspace tests, fmt, clippy `-D warnings` (44656529929)
- [x] MSRV 1.90 (44656529929)
- [x] Browser WASM + OPFS (44656529929; Tier 2 browser jobs)
- [x] Node build + client tests (44656529929)
- [x] FFI / header checks (44656529929, RN native glue)
- [x] Linux x86_64 + arm64 benchmark gates, Tier 0 fingerprints, MEM-2 (44656529929, 44657268466)
- [x] Tier 2 vs v0.13.1 + cumulative v0.12.1 (44657268466)
- [x] cargo audit, Miri, fuzz smoke (44656529929)
- [p] arm64 functional tests (emulated, K4)
- [ ] RN on device / simulator (not executed)

## Sign-offs (`V0132_FINAL_RELEASE_BLOCKERS.md` §2; no approver recorded yet)

- [~] K1: fixed, #142 merged (ASan UAF on `ae525dc` → pass); no device run
- [~] K2: fixed, #142 merged (UBSan on `ae525dc` → pass)
- [~] K3: Hermes host smoke 22/22 on #142; **iOS/Android not executed**
- [p] K4: emulated arm64 `cargo test` still running (16 binaries / 348 pass / 0 fail so far)
- [~] K5: R3 trade-off documented; gates unchanged
- [~] K6: logical contents unchanged; physical bytes change (redb metadata)
- [~] K7: **Node SIGSEGV at 10,000-level input** (measured); not fixed; needs an explicit decision
- [~] K8: Chrome/OPFS M5 evidence; damaged OPFS file untested
- [~] K9: `dev` unprotected, `main` requires 0 checks; #140, #142 and #143 all merged before CI finished; proposed commands, not applied
- [~] S1-L1: #141 misses B/D/E (documented)

## Decision

**NO-GO.** It becomes GO when every `[ ]` above is done, K4 has a result, and every `[~]` has a named sign-off.
