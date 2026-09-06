#pragma once

#include <jsi/jsi.h>
#include <string>
#include "taladb.h"

namespace taladb {

/**
 * TalaDBHostObject — JSI HostObject wrapping the Rust taladb-ffi C library.
 *
 * Installed into the JS runtime as a global:
 *   global.__TalaDB__ = <TalaDBHostObject instance>
 *
 * Every property access returns a JSI Function. `callAsync` and vector search
 * run Rust work on a bounded background executor and resolve a Promise from a
 * short JS timer; legacy synchronous methods remain for binary compatibility.
 *
 * JSON is used at the C boundary:
 *   JS object  →  JSON.stringify  →  C string  →  Rust  →  C string  →  JSON.parse  →  JS object
 */
class TalaDBHostObject : public facebook::jsi::HostObject {
public:
    explicit TalaDBHostObject(TalaDbHandle *db);
    ~TalaDBHostObject() override;

    facebook::jsi::Value get(facebook::jsi::Runtime &rt,
                             const facebook::jsi::PropNameID &name) override;

    void set(facebook::jsi::Runtime &rt,
             const facebook::jsi::PropNameID &name,
             const facebook::jsi::Value &value) override;

    std::vector<facebook::jsi::PropNameID>
    getPropertyNames(facebook::jsi::Runtime &rt) override;

    /** Install this object as global.__TalaDB__ in the given runtime. */
    static void install(facebook::jsi::Runtime &rt, TalaDbHandle *db);

private:
    TalaDbHandle *db_;

    // JSON helpers
    static std::string stringify(facebook::jsi::Runtime &rt,
                                 const facebook::jsi::Value &val);
    static facebook::jsi::Value parse(facebook::jsi::Runtime &rt,
                                      const std::string &json);

    // Convenience: convert a nullable JSI Value to a JSON C-string.
    // Returns "{}" when the value is null/undefined.
    static std::string valueToFilterJson(facebook::jsi::Runtime &rt,
                                         const facebook::jsi::Value &val);

    // ------------------------------------------------------------------
    // Float32Array helpers — zero-copy extraction from JSI
    // ------------------------------------------------------------------
    //
    // Accepts a JS `Float32Array` *or* a plain `number[]`. Populates `out`
    // with a copy of the values (kept alive by the caller for the FFI call).
    // Throws JSError when the value is neither.
    static void extractF32Query(facebook::jsi::Runtime &rt,
                                const facebook::jsi::Value &val,
                                std::vector<float> &out);

    // Poll a TalaDbJob from the JS thread via a short setTimeout until done,
    // then resolve / reject a Promise with the result / error.
    static facebook::jsi::Value awaitJobAsPromise(facebook::jsi::Runtime &rt,
                                                  TalaDbJob *job,
                                                  bool parseAsJson);
};

} // namespace taladb
