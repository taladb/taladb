# v0.13.2 — Final release blockers (M5.1)

| | |
|---|---|
| Release candidate | **`dev` `1e8129c`**: M2–M5 + #141 + #142 (RN fixes) + #143 (CHANGELOG + bump), all merged by the maintainer |
| Candidate CI | push CI **44656529929** 16/16 green; Tier 2 **44657268466** green |
| Docs follow-up | **#144** merged 02:53:08Z (`a461176`): CHANGELOG numbers from the candidate run + Known regressions |
| Release PR `dev` → `main` | **not opened**: its prerequisites are not met (§4) |

**Status words:** *Verified* = run in M5.1 and seen. *Pending* = started, no result. *Not executed* = nobody ran it.

**Decision: NO-GO.** The code is merged and green on the candidate. But nothing is signed off: every K item and S1-L1 still needs a named approver (§2). K4 has no complete result.

## 1. Must fix (code and documentation)

| ID | Item | State |
|---|---|---|
| B1 | S1-residual (plain open of a ≤ 0.13.1-damaged file) | **Done.** #141 merged; `dev` CI 16/16 green on `ae525dc`. Reviewed in M5.1 (§3). |
| B2 | `CHANGELOG.md` 0.13.2 entry with the repair recipe | **Merged in #143**; number corrections and Known regressions merged in **#144**. The recipe was compiled and run verbatim against damaged fixtures: it repairs both shapes, and refuses mixed files and wrong passphrases without writing. |
| B3 | Version bump 0.13.1 → 0.13.2 | **Done in #143**, using the repository's own `scripts/sync-version.js` (same 11 files as the 0.13.1 bump `427b00f`); `sync-version.js --check 0.13.2` passes. **Merged.** |
| B4 (new) | **K1 use-after-free in the RN HostObject** (reachable from app JS) | **Fixed in #142 (merged).** Reproduced with ASan on `ae525dc`; passes on #142. |
| B5 (new) | **`TalaDBModule.close()` never closed the database on iOS/Android**: an immediate re-`initialize()` of the same file failed with "Database already open" | **Fixed in #142.** Found while preparing K3. Reproduced on `dev`, even after a forced GC; passes on #142. Merged. |

## 2. Sign-off decisions

No approver is recorded for any item. The "Proposed decision" column is a recommendation, not an approval.

| ID | Evidence | Risk | Proposed decision | Approver |
|---|---|---|---|---|
| **K1** RN raw-`this` lifetime | Hermes 0.14.1 + ASan: **heap-use-after-free on `dev`** (freed by HadesGC young-gen collection, read by a retained `find`). On #142: method keeps the host alive, works, fails cleanly after `close()`. | High on `dev` (memory corruption from ordinary JS such as `const { find } = __TalaDB__`); Low with #142 | **Done (#142 merged).** Residual: not run on a device | Maintainer (Dennis Paler): **pending** |
| **K2** RN numeric conversions | UBSan on `dev`: `nan is outside the range of representable values` at all four cast sites; `setUserVersion(-1)` silently accepted; `setUserVersion("3")` aborts on a JSI assertion (exit 134). On #142: 26/26 checks throw JS errors; in-range values unchanged. | Medium on `dev` (UB, app-controlled input); Low with #142 | **Done (#142 merged)** | Maintainer: **pending** |
| **K3** RN runtime smoke | **Partly executed.** Host harness (real `TalaDBHostObject.cpp` in Hermes 0.14.1 on macOS, real `libtaladb_ffi`, install/close steps mirroring JNI/iOS): open, insert, query, index, userVersion, async (callAsync/findAsync), close, stale handle, reopen, plain open of encrypted refused, wrong passphrase refused, correct passphrase recovers data, passphrase on plaintext refused with no salt invented. **#142: 22/22. `dev`: 3 failures (B5).** **Not executed:** iOS simulator or Android (no Android SDK here; the iOS simulator toolchain exists but there is no RN app, and 4.5 GB of disk is free); `TalaDB.mm` is never compiled anywhere. | Medium. The host run already found B5; device-only code (`TalaDB.mm`, Kotlin, the RN bridge) is unexercised | Run one iOS simulator smoke on #142 before tagging, **or** accept explicitly | Maintainer: **pending** |
| **K4** Linux arm64 functional tests | Emulated `linux/arm64` (qemu, `rust:1.98-slim-bookworm`) run of `cargo test -p taladb --all-features` on a tree identical to `ae525dc`'s `packages/` (diffed). **Pending: still compiling at the time of writing, no test result.** CI runs Tier 0 / Criterion / Vector on real arm64, all green. | Low (same code is green on x86_64 and in arm64 benchmark fingerprints) | Accept only once the emulated result is read; add an arm64 test job later | Maintainer: **pending** |
| **K5** R3 write trade-off | Local M5 sweep: mem 4R+2W writes/s ×0.56, write p99 ×4.3, reads/s ×4.5; file writes/s ×1.02, write p99 ×1.35–2.5. Tier 2 gates unchanged and green (run 44639073531). Documented in the CHANGELOG. | Low–Medium (in-memory, read-saturated workloads) | Accept as documented | Maintainer: **pending** |
| **K6** Rejected open rewrites file bytes | Logical contents identical (every table and key/value pair, fingerprint tests). The physical bytes change because redb rewrites its header/allocator state and trims the file on every open + close; a successful no-op open changes them the same way (M5 control). | Low: storage-engine behaviour, not a data change | Accept. Explanation in `V0132_FINAL_RELEASE_READINESS.md` §4 | Maintainer: **pending** |
| **K7** Node input nesting | **Measured in M5.1:** a 6,000-level JS object is refused; a **10,000-level object kills the Node process (SIGSEGV, exit 139)** inside napi's recursive JS → `serde_json::Value` conversion, before TalaDB's 64-level check. V8 `JSON.parse` takes that depth without trouble, so a Node server storing a client's parsed JSON can be crashed by one ~50 KB request. Pre-existing (0.13.1 is the same). Browser, RN (Hermes stringify cap, verified) and FFI refuse such input. | **Medium** (remote DoS for Node server deployments) | No minimal fix: it needs a depth-checked `FromNapiValue` type on ~20 napi signatures plus generated TS types. **Accept for 0.13.2** with the CHANGELOG warning (written), fix in 0.13.3 — **or** hold the release for that fix | Maintainer: **pending — required** |
| **K8** Chrome/OPFS encryption | M5, real Chrome: plain open of an encrypted DB refused; wrong passphrase refused; correct passphrase opens all data afterwards; encrypted second tab refused; e2e 93/93. CI: browser phases 1, 2, 3, 6. **Gap:** no S1-damaged OPFS file (cannot be created through the browser API); the wrong-passphrase message is a bare `aead::Error` (cosmetic, in the CHANGELOG). Nothing new run in M5.1. | Low | Accept | Maintainer: **pending** |
| **K9** Branch protection | `dev`: **none** (`GET …/branches/dev/protection` → 404). `main`: 1 approving review, signed commits, **0 required status checks**, `enforce_admins` off, force pushes **allowed**. | Medium (#132/#133/#140 merged before CI finished) | Before the release PR: require `CI — all checks passed` on `dev` and `main`, and disable force pushes on `main` (command in the readiness report §5). **Not applied**: a repository settings change needs your explicit go-ahead | Maintainer: **pending** |
| **S1-L1 (new)** #141 detection limits | M5.1 probes (`m51-evidence/s1-probes`): **caught**: plaintext after the ciphertext in a collection (A); a derived `_id` sorted first in 0/64 trials (C). **Not caught (open succeeds, as in 0.13.1):** plaintext whose `_id` sorts before every encrypted document (B; a public-API insert then succeeds, F); a damaged DB with no documents (D) or with all documents deleted (E). In B, D and E the passphrase open says "not encrypted". The strict repair refuses B (writes nothing) and repairs D and E. | Low–Medium: damage needs a ≤ 0.13.1 plain open; B needs an old/synced `_id`; no regression vs 0.13.1 | Accept as documented in the CHANGELOG; widen detection in 0.13.3 | Maintainer: **pending** |

## 3. PR #141 review (verified)

- **Code:** `refuse_ciphertext_documents` runs only when the version record decodes (`StoredState::Readable`). It reads the first value of every `docs::` table and refuses values ≥ 28 bytes that lack the plaintext header (postcard length 26 + 26 Crockford base32 chars). Error results from the scan are propagated except `Encryption`, which is ignored on purpose (tampering inside `EncryptedBackend`). No `unwrap`, indexing or arithmetic on stored bytes (`get(..27)` with a slice pattern). No format or ABI change.
- **Assumptions checked:**
  - Every plaintext document starts with that header. The id is always a ULID (`take_supplied_id` rejects anything else).
  - Ciphertext starts with `0x01`, or a random nonce before 0.7.3; a random nonce matching the header has probability ≈ 2⁻⁸·(1/8)²⁶.
  - The 28-byte minimum matches `MIN_STORED_CIPHERTEXT`, the constant `stored_state` already uses.
- **Order assumption:** the check sees only the first key of each collection. Keys are 16-byte ULIDs, so time-ordered ids put post-damage plaintext *after* the ciphertext (caught). Caller-supplied, synced or derived ids can sort first (not caught; S1-L1).
- **Compatibility:** plaintext, encrypted (native and custom backend), legacy (no version record: unaffected, because that path goes through `probe_documents`) and deeply nested databases open as before. Covered by #141's tests and the `deep_value_in_stored_document_body_*` tests in `dev` CI.
- **Verdict:** correct for what it claims. It was already merged by the maintainer before this review; the post-merge `dev` CI is green.

## 4. Before the `dev` → `main` release PR

1. ~~Merge #142, then #143~~: done by the maintainer, **before their PR CI finished** (it passed afterwards).
2. ~~`dev` CI + Tier 2 on the merged SHA~~: done for `1e8129c` (44656529929, 44657268466), both green. #144 (`a461176`) changed only `CHANGELOG.md`; `dev` CI was skipped for it, and the source tested is unchanged from `1e8129c`.
3. K4 result read; named sign-off on K1–K9 and S1-L1 (K7 explicitly).
4. Branch protection (K9), if approved.
5. Set the CHANGELOG date at tag time ("Unreleased" now).

## 5. Deferred (unchanged from M5)

S6 rest, S-A1, S7, S8–S10, R5 (OPFS short writes; first item for 0.13.3), R6–R11, M3-N1/N2/N3/N4/N5, M5-N1, M5-N2 (no repair API in bindings). New for 0.13.3: K7 napi depth-checked input; S1-L1 wider detection; an arm64 test job; the Hermes host harness as a CI job.
