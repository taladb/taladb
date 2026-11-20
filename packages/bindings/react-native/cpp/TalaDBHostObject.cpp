#include "TalaDBHostObject.h"

#include <memory>
#include <cmath>
#include <cstring>
#include <limits>
#include <stdexcept>
#include <string>
#include <type_traits>
#include <vector>

using namespace facebook::jsi;

namespace taladb {

namespace {

// Read the error string set by the most recent FFI call. Safe to call even
// when no error has been set — returns an empty string in that case.
std::string lastError() {
    const char *msg = taladb_last_error();
    return msg ? std::string(msg) : std::string();
}

// Build a JSError with the FFI last-error message appended.
JSError ffiError(Runtime &rt, const char *prefix) {
    std::string msg = prefix;
    std::string last = lastError();
    if (!last.empty()) {
        msg += ": ";
        msg += last;
    }
    return JSError(rt, msg);
}

// Convert a JS number to an unsigned integer for the C API. A C++ cast from a
// double is undefined behaviour for NaN, an infinity, or a value whose integer
// part does not fit the target, so those are refused with a JS error. Every
// other number converts exactly as the cast did: fractions truncate toward
// zero, so -0.5 and 0.5 both give 0.
template <typename T>
T toUnsigned(Runtime &rt, const Value &v, const char *what) {
    static_assert(std::is_unsigned<T>::value, "unsigned targets only");
    if (!v.isNumber()) {
        throw JSError(rt, std::string(what) + " must be a number");
    }
    const double d = v.getNumber();
    // 2^digits is exact in a double, and the integer part of every double below
    // it fits T. The comparisons are false for NaN.
    const double limit = std::ldexp(1.0, std::numeric_limits<T>::digits);
    if (!(d > -1.0 && d < limit)) {
        throw JSError(rt, std::string(what) + " must be a non-negative integer below 2^" +
                              std::to_string(std::numeric_limits<T>::digits));
    }
    return static_cast<T>(d);
}

// A double outside float's finite range has no float to convert to (undefined
// behaviour in C++). NaN and the infinities convert as before.
float toFloat(Runtime &rt, double d) {
    if (std::isfinite(d) && std::fabs(d) > static_cast<double>(std::numeric_limits<float>::max())) {
        throw JSError(rt, "query vector value is outside the 32-bit float range");
    }
    return static_cast<float>(d);
}

} // namespace

// ---------------------------------------------------------------------------
// Constructor / Destructor
// ---------------------------------------------------------------------------

TalaDBHostObject::TalaDBHostObject(TalaDbHandle *db) : db_(db) {}

TalaDBHostObject::~TalaDBHostObject() {
    if (db_) {
        taladb_close(db_);
        db_ = nullptr;
    }
}

// ---------------------------------------------------------------------------
// Static installer
// ---------------------------------------------------------------------------

void TalaDBHostObject::install(Runtime &rt, TalaDbHandle *db) {
    auto hostObject = std::make_shared<TalaDBHostObject>(db);
    auto jsiObject  = Object::createFromHostObject(rt, hostObject);
    rt.global().setProperty(rt, "__TalaDB__", std::move(jsiObject));
}

void TalaDBHostObject::uninstall(Runtime &rt) {
    // Our own `close` never throws. The global is writable from JS, so it may
    // hold something else whose `close` does; that must not escape into the
    // JNI / Objective-C caller, and the global is cleared either way.
    try {
        auto current = rt.global().getProperty(rt, "__TalaDB__");
        if (current.isObject()) {
            auto close = current.getObject(rt).getProperty(rt, "close");
            if (close.isObject() && close.getObject(rt).isFunction(rt)) {
                close.getObject(rt).getFunction(rt).call(rt);
            }
        }
    } catch (const JSIException &) {
    }
    rt.global().setProperty(rt, "__TalaDB__", Value::undefined());
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

std::string TalaDBHostObject::stringify(Runtime &rt, const Value &val) {
    auto json    = rt.global().getPropertyAsObject(rt, "JSON");
    auto strFn   = json.getPropertyAsFunction(rt, "stringify");
    auto result  = strFn.call(rt, val);
    if (result.isString()) {
        return result.getString(rt).utf8(rt);
    }
    return "null";
}

Value TalaDBHostObject::parse(Runtime &rt, const std::string &json) {
    auto jsonObj = rt.global().getPropertyAsObject(rt, "JSON");
    auto parseFn = jsonObj.getPropertyAsFunction(rt, "parse");
    return parseFn.call(rt, String::createFromUtf8(rt, json));
}

std::string TalaDBHostObject::valueToFilterJson(Runtime &rt, const Value &val) {
    if (val.isNull() || val.isUndefined()) {
        return "{}";
    }
    return stringify(rt, val);
}

// ---------------------------------------------------------------------------
// Float32Array fast path
// ---------------------------------------------------------------------------

void TalaDBHostObject::extractF32Query(Runtime &rt, const Value &val,
                                       std::vector<float> &out) {
    if (!val.isObject()) {
        throw JSError(rt, "query must be a Float32Array or number[]");
    }
    auto obj = val.getObject(rt);

    // --- Fast path: Float32Array — zero-copy read from the underlying ArrayBuffer ---
    if (obj.isArrayBuffer(rt)) {
        // Raw ArrayBuffer of bytes (length must be a multiple of 4)
        auto ab     = obj.getArrayBuffer(rt);
        size_t size = ab.size(rt);
        if (size % sizeof(float) != 0) {
            throw JSError(rt, "ArrayBuffer length is not a multiple of 4 bytes");
        }
        size_t n = size / sizeof(float);
        out.resize(n);
        if (size != 0) std::memcpy(out.data(), ab.data(rt), size);
        return;
    }

    // Float32Array exposes `.buffer`, `.byteOffset`, `.byteLength`. We read
    // those directly and memcpy from the underlying ArrayBuffer — no JSON,
    // no per-element Value conversion.
    auto arrayBuffer = rt.global().getPropertyAsObject(rt, "ArrayBuffer");
    auto isView = arrayBuffer.getPropertyAsFunction(rt, "isView").call(rt, obj);
    if (isView.isBool() && isView.getBool()) {
        auto float32 = rt.global().getPropertyAsFunction(rt, "Float32Array");
        if (obj.instanceOf(rt, float32)) {
            auto buf      = obj.getPropertyAsObject(rt, "buffer");
            auto ab       = buf.getArrayBuffer(rt);
            auto offsetV  = obj.getProperty(rt, "byteOffset");
            auto lengthV  = obj.getProperty(rt, "byteLength");
            auto checkedSize = [&rt](const Value &v) -> size_t {
                if (!v.isNumber()) throw JSError(rt, "invalid typed-array byte range");
                double n = v.getNumber();
                if (!std::isfinite(n) || n < 0 || std::floor(n) != n ||
                    n >= static_cast<double>(std::numeric_limits<size_t>::max())) {
                    throw JSError(rt, "invalid typed-array byte range");
                }
                return static_cast<size_t>(n);
            };
            size_t offset = checkedSize(offsetV);
            size_t length = checkedSize(lengthV);
            if (offset > ab.size(rt) || length > ab.size(rt) - offset) {
                throw JSError(rt, "typed-array byte range exceeds its buffer");
            }
            if (length % sizeof(float) != 0) {
                throw JSError(rt, "typed-array byteLength is not a multiple of 4");
            }
            size_t n = length / sizeof(float);
            out.resize(n);
            if (length != 0) std::memcpy(out.data(), ab.data(rt) + offset, length);
            return;
        }
        throw JSError(rt, "query typed array must be Float32Array");
    }

    // --- Fallback: number[] — per-element conversion (slow, but correct) ---
    if (obj.isArray(rt)) {
        auto arr   = obj.getArray(rt);
        size_t len = arr.size(rt);
        out.resize(len);
        for (size_t i = 0; i < len; i++) {
            auto v = arr.getValueAtIndex(rt, i);
            if (!v.isNumber()) {
                throw JSError(rt, "query array must contain only numbers");
            }
            out[i] = toFloat(rt, v.getNumber());
        }
        return;
    }

    throw JSError(rt, "query must be a Float32Array, typed array, or number[]");
}

// ---------------------------------------------------------------------------
// Async job → Promise bridge
//
// JSI on RN does not give us a thread-safe way to resolve a Promise from a
// background OS thread. We poll the job handle from the JS thread using
// a bounded timer. The DB work itself runs on the background worker the FFI
// spawned; idle polling yields to rendering instead of spinning on setImmediate.
// ---------------------------------------------------------------------------

Value TalaDBHostObject::awaitJobAsPromise(Runtime &rt, TalaDbJob *job, bool parseAsJson) {
    if (!job) {
        throw ffiError(rt, "failed to start background job");
    }

    auto promiseCtor = rt.global().getPropertyAsFunction(rt, "Promise");

    // The job pointer is owned by the executor closure; we null it out on
    // consumption to prevent double-free.
    auto jobBox = std::shared_ptr<TalaDbJob *>(new TalaDbJob *(job), [](TalaDbJob **slot) {
        if (*slot) taladb_job_cancel(*slot);
        delete slot;
    });

    auto executor = Function::createFromHostFunction(
        rt, PropNameID::forAscii(rt, "taladbJobExecutor"), 2,
        [jobBox, parseAsJson](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
            if (count < 2) {
                return Value::undefined();
            }
            auto resolve = std::make_shared<Function>(args[0].getObject(rt).getFunction(rt));
            auto reject  = std::make_shared<Function>(args[1].getObject(rt).getFunction(rt));

            // `setTimeout` is provided by React Native. Reschedule until the
            // worker thread finishes, then take the result on the JS thread.
            auto timer = rt.global().getPropertyAsFunction(rt, "setTimeout");
            auto timerShared = std::make_shared<Function>(std::move(timer));

            // Only the pending JS tick owns the poller; the callable itself
            // keeps a weak reference so completion releases all JSI captures.
            auto poller = std::make_shared<std::function<void(Runtime &)>>();
            std::weak_ptr<std::function<void(Runtime &)>> weakPoller = poller;
            *poller = [jobBox, resolve, reject, timerShared, weakPoller, parseAsJson](Runtime &rt) {
                TalaDbJob *j = *jobBox;
                if (!j) {
                    return; // already consumed — defensive
                }
                int32_t state = taladb_job_poll(j);
                if (state == 0) {
                    // Still running — reschedule.
                    auto poller = weakPoller.lock();
                    if (!poller) return;
                    auto tick = Function::createFromHostFunction(
                        rt, PropNameID::forAscii(rt, "taladbJobTick"), 0,
                        [poller](Runtime &rt, const Value &, const Value *, size_t) -> Value {
                            (*poller)(rt);
                            return Value::undefined();
                        });
                    timerShared->call(rt, tick, 4.0);
                    return;
                }

                // Done — take the result and clear the job slot.
                char *raw = taladb_job_take_result(j);
                *jobBox = nullptr;

                if (!raw) {
                    std::string last = lastError();
                    auto err = String::createFromUtf8(
                        rt, last.empty() ? "taladb job failed" : last);
                    reject->call(rt, err);
                    return;
                }

                std::string json(raw);
                taladb_free_string(raw);

                if (parseAsJson) {
                    try {
                        resolve->call(rt, parse(rt, json));
                    } catch (const JSError &e) {
                        reject->call(rt, String::createFromUtf8(rt, e.getMessage()));
                    }
                } else {
                    resolve->call(rt, String::createFromUtf8(rt, json));
                }
            };

            (*poller)(rt);
            return Value::undefined();
        });

    return promiseCtor.callAsConstructor(rt, executor);
}

// ---------------------------------------------------------------------------
// Property names advertised to JS
// ---------------------------------------------------------------------------

std::vector<PropNameID> TalaDBHostObject::getPropertyNames(Runtime &rt) {
    std::vector<std::string> names = {
        "insert", "insertMany",
        "find", "findOne",
        "updateOne", "updateMany",
        "deleteOne", "deleteMany",
        "count",
        "aggregate",
        "listCollectionNames",
        "userVersion", "setUserVersion",
        "flush",
        "createIndex", "dropIndex",
        "createCompoundIndex", "dropCompoundIndex",
        "createFtsIndex", "dropFtsIndex",
        "createVectorIndex", "dropVectorIndex", "upgradeVectorIndex",
        "findNearest", "findNearestAsync",
        "searchText", "hybridSearch",
        "findAsync", "callAsync",
        "compact",
        "close",
    };
    std::vector<PropNameID> result;
    result.reserve(names.size());
    for (auto &n : names) {
        result.push_back(PropNameID::forUtf8(rt, n));
    }
    return result;
}

void TalaDBHostObject::set(Runtime &, const PropNameID &, const Value &) {}

// ---------------------------------------------------------------------------
// Property dispatch
// ---------------------------------------------------------------------------

Value TalaDBHostObject::get(Runtime &rt, const PropNameID &propName) {
    auto name = propName.utf8(rt);
    // Each returned function keeps this object alive (see the header).
    auto self = shared_from_this();

    if (name == "callAsync") {
        return Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "callAsync"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "callAsync requires an operation and argument array");
                auto op = args[0].getString(rt).utf8(rt);
                auto json = stringify(rt, args[1]);
                return awaitJobAsPromise(rt, taladb_call_start(self->db_, op.c_str(), json.c_str()), true);
            });
    }
    // ------------------------------------------------------------------
    // insert(collection: string, doc: object): string
    // ------------------------------------------------------------------
    if (name == "insert") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "insert"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "insert requires 2 arguments");
                auto col     = args[0].getString(rt).utf8(rt);
                auto docJson = stringify(rt, args[1]);
                char *result = taladb_insert(self->db_, col.c_str(), docJson.c_str());
                if (!result) throw JSError(rt, "taladb_insert failed");
                std::string id(result);
                taladb_free_string(result);
                return String::createFromUtf8(rt, id);
            });
    }

    // ------------------------------------------------------------------
    // insertMany(collection: string, docs: object[]): string[]
    // ------------------------------------------------------------------
    if (name == "insertMany") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "insertMany"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "insertMany requires 2 arguments");
                auto col      = args[0].getString(rt).utf8(rt);
                auto docsJson = stringify(rt, args[1]);
                char *result  = taladb_insert_many(self->db_, col.c_str(), docsJson.c_str());
                if (!result) throw JSError(rt, "taladb_insert_many failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // find(collection: string, filter: object | null): object[]
    // ------------------------------------------------------------------
    if (name == "find") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "find"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 1) throw JSError(rt, "find requires at least 1 argument");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = count > 1 ? valueToFilterJson(rt, args[1]) : "{}";
                char *result    = taladb_find(self->db_, col.c_str(), filterJson.c_str());
                if (!result) throw JSError(rt, "taladb_find failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // findOne(collection: string, filter: object | null): object | null
    // ------------------------------------------------------------------
    if (name == "findOne") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "findOne"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 1) throw JSError(rt, "findOne requires at least 1 argument");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = count > 1 ? valueToFilterJson(rt, args[1]) : "{}";
                char *result    = taladb_find_one(self->db_, col.c_str(), filterJson.c_str());
                if (!result) throw JSError(rt, "taladb_find_one failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // updateOne(collection, filter, update): boolean
    // ------------------------------------------------------------------
    if (name == "updateOne") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "updateOne"), 3,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 3) throw JSError(rt, "updateOne requires 3 arguments");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = stringify(rt, args[1]);
                auto updateJson = stringify(rt, args[2]);
                int32_t res = taladb_update_one(
                    self->db_, col.c_str(), filterJson.c_str(), updateJson.c_str());
                if (res < 0) throw JSError(rt, "taladb_update_one failed");
                return Value(res == 1);
            });
    }

    // ------------------------------------------------------------------
    // updateMany(collection, filter, update): number
    // ------------------------------------------------------------------
    if (name == "updateMany") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "updateMany"), 3,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 3) throw JSError(rt, "updateMany requires 3 arguments");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = stringify(rt, args[1]);
                auto updateJson = stringify(rt, args[2]);
                int32_t res = taladb_update_many(
                    self->db_, col.c_str(), filterJson.c_str(), updateJson.c_str());
                if (res < 0) throw JSError(rt, "taladb_update_many failed");
                return Value(static_cast<double>(res));
            });
    }

    // ------------------------------------------------------------------
    // deleteOne(collection, filter): boolean
    // ------------------------------------------------------------------
    if (name == "deleteOne") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "deleteOne"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "deleteOne requires 2 arguments");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = stringify(rt, args[1]);
                int32_t res = taladb_delete_one(self->db_, col.c_str(), filterJson.c_str());
                if (res < 0) throw JSError(rt, "taladb_delete_one failed");
                return Value(res == 1);
            });
    }

    // ------------------------------------------------------------------
    // deleteMany(collection, filter): number
    // ------------------------------------------------------------------
    if (name == "deleteMany") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "deleteMany"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "deleteMany requires 2 arguments");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = stringify(rt, args[1]);
                int32_t res = taladb_delete_many(self->db_, col.c_str(), filterJson.c_str());
                if (res < 0) throw JSError(rt, "taladb_delete_many failed");
                return Value(static_cast<double>(res));
            });
    }

    // ------------------------------------------------------------------
    // count(collection, filter): number
    // ------------------------------------------------------------------
    if (name == "count") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "count"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 1) throw JSError(rt, "count requires at least 1 argument");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = count > 1 ? valueToFilterJson(rt, args[1]) : "{}";
                int32_t res = taladb_count(self->db_, col.c_str(), filterJson.c_str());
                if (res < 0) throw JSError(rt, "taladb_count failed");
                return Value(static_cast<double>(res));
            });
    }

    // ------------------------------------------------------------------
    // aggregate(collection: string, pipeline: object[]): object[]
    // ------------------------------------------------------------------
    if (name == "aggregate") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "aggregate"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "aggregate requires 2 arguments");
                auto col          = args[0].getString(rt).utf8(rt);
                auto pipelineJson = stringify(rt, args[1]);
                char *result      = taladb_aggregate(self->db_, col.c_str(), pipelineJson.c_str());
                if (!result) throw JSError(rt, "taladb_aggregate failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // Collection metadata
    // ------------------------------------------------------------------
    if (name == "listCollectionNames") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "listCollectionNames"), 0,
            [self](Runtime &rt, const Value &, const Value *, size_t) -> Value {
                char *result = taladb_list_collection_names(self->db_);
                if (!result) throw JSError(rt, "taladb_list_collection_names failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);  // JSON array → JS string[]
            });
    }

    // ------------------------------------------------------------------
    // Application migrations — userVersion / setUserVersion
    // ------------------------------------------------------------------
    if (name == "userVersion") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "userVersion"), 0,
            [self](Runtime &rt, const Value &, const Value *, size_t) -> Value {
                int64_t v = taladb_user_version(self->db_);
                if (v < 0) throw JSError(rt, "taladb_user_version failed");
                return Value(static_cast<double>(v));
            });
    }

    if (name == "setUserVersion") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "setUserVersion"), 1,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 1) throw JSError(rt, "setUserVersion requires 1 argument");
                uint32_t version = toUnsigned<uint32_t>(rt, args[0], "version");
                if (taladb_set_user_version(self->db_, version) < 0)
                    throw JSError(rt, "taladb_set_user_version failed");
                return Value::undefined();
            });
    }

    if (name == "flush") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "flush"), 0,
            [self](Runtime &rt, const Value &, const Value *, size_t) -> Value {
                if (taladb_flush(self->db_) < 0) throw JSError(rt, "taladb_flush failed");
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // createIndex / dropIndex / createFtsIndex / dropFtsIndex
    // ------------------------------------------------------------------
    if (name == "createIndex" || name == "dropIndex" ||
        name == "createFtsIndex" || name == "dropFtsIndex") {
        return Function::createFromHostFunction(
            rt, PropNameID::forUtf8(rt, name), 2,
            [self, name](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, (name + " requires 2 arguments").c_str());
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                int32_t res;
                if      (name == "createIndex")    res = taladb_create_index    (self->db_, col.c_str(), field.c_str());
                else if (name == "dropIndex")      res = taladb_drop_index      (self->db_, col.c_str(), field.c_str());
                else if (name == "createFtsIndex") res = taladb_create_fts_index(self->db_, col.c_str(), field.c_str());
                else                               res = taladb_drop_fts_index  (self->db_, col.c_str(), field.c_str());
                if (res < 0) throw ffiError(rt, (name + " failed").c_str());
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // createCompoundIndex / dropCompoundIndex(collection, fields: string[]): void
    // ------------------------------------------------------------------
    if (name == "createCompoundIndex" || name == "dropCompoundIndex") {
        return Function::createFromHostFunction(
            rt, PropNameID::forUtf8(rt, name), 2,
            [self, name](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, (name + " requires 2 arguments").c_str());
                auto col        = args[0].getString(rt).utf8(rt);
                auto fieldsJson = stringify(rt, args[1]);   // string[] → JSON array
                int32_t res = name == "createCompoundIndex"
                    ? taladb_create_compound_index(self->db_, col.c_str(), fieldsJson.c_str())
                    : taladb_drop_compound_index(self->db_, col.c_str(), fieldsJson.c_str());
                if (res < 0) throw ffiError(rt, (name + " failed").c_str());
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // createVectorIndex(collection, field, dimensions, opts?): void
    //   opts: { metric?: 'cosine'|'dot'|'euclidean', hnsw?: HnswOptions }
    // ------------------------------------------------------------------
    if (name == "createVectorIndex") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "createVectorIndex"), 4,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 3) throw JSError(rt, "createVectorIndex requires (collection, field, dimensions, opts?)");
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                size_t dims = toUnsigned<size_t>(rt, args[2], "dimensions");

                std::string metricStr;
                std::string hnswJson;
                const char *metricPtr = nullptr;
                const char *hnswPtr   = nullptr;
                if (count > 3 && args[3].isObject()) {
                    auto opts = args[3].getObject(rt);
                    if (opts.hasProperty(rt, "metric")) {
                        auto m = opts.getProperty(rt, "metric");
                        if (m.isString()) {
                            metricStr = m.getString(rt).utf8(rt);
                            metricPtr = metricStr.c_str();
                        }
                    }
                    if (opts.hasProperty(rt, "hnsw")) {
                        auto h = opts.getProperty(rt, "hnsw");
                        if (h.isObject()) {
                            hnswJson = stringify(rt, h);
                            hnswPtr  = hnswJson.c_str();
                        }
                    }
                }

                int32_t res = taladb_create_vector_index(
                    self->db_, col.c_str(), field.c_str(), dims, metricPtr, hnswPtr);
                if (res < 0) throw ffiError(rt, "taladb_create_vector_index failed");
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // dropVectorIndex(collection, field): void
    // ------------------------------------------------------------------
    if (name == "dropVectorIndex") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "dropVectorIndex"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "dropVectorIndex requires 2 arguments");
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                int32_t res = taladb_drop_vector_index(self->db_, col.c_str(), field.c_str());
                if (res < 0) throw ffiError(rt, "taladb_drop_vector_index failed");
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // upgradeVectorIndex(collection, field): void
    // ------------------------------------------------------------------
    if (name == "upgradeVectorIndex") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "upgradeVectorIndex"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 2) throw JSError(rt, "upgradeVectorIndex requires 2 arguments");
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                int32_t res = taladb_upgrade_vector_index(self->db_, col.c_str(), field.c_str());
                if (res < 0) throw ffiError(rt, "taladb_upgrade_vector_index failed");
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // rebuildVectorIndexes(): void
    //
    // Warms every HNSW graph in the database. The graphs are memory-only, so
    // without this findNearest silently degrades to an exact linear scan for
    // the whole process lifetime. Scans every indexed vector — call it once at
    // startup, off the first frame.
    // ------------------------------------------------------------------
    if (name == "rebuildVectorIndexes") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "rebuildVectorIndexes"), 0,
            [self](Runtime &rt, const Value &, const Value *, size_t) -> Value {
                int32_t res = taladb_rebuild_hnsw_indexes(self->db_);
                if (res < 0) throw ffiError(rt, "taladb_rebuild_hnsw_indexes failed");
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // findNearest(collection, field, query, topK, filter?): { document, score }[]
    //   query — Float32Array (preferred, zero-copy) or number[]
    // ------------------------------------------------------------------
    if (name == "findNearest") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "findNearest"), 5,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 4) throw JSError(rt, "findNearest requires (collection, field, query, topK, filter?)");
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                std::vector<float> qbuf;
                extractF32Query(rt, args[2], qbuf);
                size_t topK = toUnsigned<size_t>(rt, args[3], "topK");
                std::string filterJson = count > 4
                    ? valueToFilterJson(rt, args[4])
                    : std::string();
                const char *filterPtr = count > 4 ? filterJson.c_str() : nullptr;

                char *result = taladb_find_nearest(
                    self->db_, col.c_str(), field.c_str(),
                    qbuf.data(), qbuf.size(), topK, filterPtr);
                if (!result) throw ffiError(rt, "taladb_find_nearest failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // findNearestAsync(...) → Promise<{ document, score }[]>
    //   Runs on a background thread. JS thread polls via a bounded timer.
    // ------------------------------------------------------------------
    if (name == "findNearestAsync") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "findNearestAsync"), 5,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 4) throw JSError(rt, "findNearestAsync requires (collection, field, query, topK, filter?)");
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                std::vector<float> qbuf;
                extractF32Query(rt, args[2], qbuf);
                size_t topK = toUnsigned<size_t>(rt, args[3], "topK");
                std::string filterJson = count > 4
                    ? valueToFilterJson(rt, args[4])
                    : std::string();
                const char *filterPtr = count > 4 ? filterJson.c_str() : nullptr;

                TalaDbJob *job = taladb_find_nearest_start(
                    self->db_, col.c_str(), field.c_str(),
                    qbuf.data(), qbuf.size(), topK, filterPtr);
                return awaitJobAsPromise(rt, job, /*parseAsJson=*/true);
            });
    }

    // ------------------------------------------------------------------
    // searchText(collection, field, query, topK, filter?, options?)
    //   → { document, score }[]
    // ------------------------------------------------------------------
    if (name == "searchText") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "searchText"), 6,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 4) throw JSError(rt, "searchText requires (collection, field, query, topK, filter?, options?)");
                auto col   = args[0].getString(rt).utf8(rt);
                auto field = args[1].getString(rt).utf8(rt);
                auto query = args[2].getString(rt).utf8(rt);
                size_t topK = toUnsigned<size_t>(rt, args[3], "topK");

                std::string filterJson = count > 4 ? valueToFilterJson(rt, args[4]) : std::string();
                const char *filterPtr = count > 4 && !args[4].isNull() && !args[4].isUndefined()
                    ? filterJson.c_str() : nullptr;
                std::string optionsJson = count > 5 && args[5].isObject() ? stringify(rt, args[5]) : std::string();
                const char *optionsPtr = !optionsJson.empty() ? optionsJson.c_str() : nullptr;

                char *result = taladb_search_text(
                    self->db_, col.c_str(), field.c_str(), query.c_str(), topK, filterPtr, optionsPtr);
                if (!result) throw ffiError(rt, "taladb_search_text failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // hybridSearch(collection, textField, text, vectorField, vector, topK,
    //              filter?, options?) → { document, score, textRank, vectorRank }[]
    //   vector — Float32Array (preferred, zero-copy) or number[]
    // ------------------------------------------------------------------
    if (name == "hybridSearch") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "hybridSearch"), 8,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 6) throw JSError(rt, "hybridSearch requires (collection, textField, text, vectorField, vector, topK, filter?, options?)");
                auto col   = args[0].getString(rt).utf8(rt);
                auto tf    = args[1].getString(rt).utf8(rt);
                auto text  = args[2].getString(rt).utf8(rt);
                auto vf    = args[3].getString(rt).utf8(rt);
                std::vector<float> vbuf;
                extractF32Query(rt, args[4], vbuf);
                size_t topK = toUnsigned<size_t>(rt, args[5], "topK");

                std::string filterJson = count > 6 ? valueToFilterJson(rt, args[6]) : std::string();
                const char *filterPtr = count > 6 && !args[6].isNull() && !args[6].isUndefined()
                    ? filterJson.c_str() : nullptr;
                std::string optionsJson = count > 7 && args[7].isObject() ? stringify(rt, args[7]) : std::string();
                const char *optionsPtr = !optionsJson.empty() ? optionsJson.c_str() : nullptr;

                char *result = taladb_hybrid_search(
                    self->db_, col.c_str(), tf.c_str(), text.c_str(), vf.c_str(),
                    vbuf.data(), vbuf.size(), topK, filterPtr, optionsPtr);
                if (!result) throw ffiError(rt, "taladb_hybrid_search failed");
                std::string json(result);
                taladb_free_string(result);
                return parse(rt, json);
            });
    }

    // ------------------------------------------------------------------
    // findAsync(collection, filter?) → Promise<object[]>
    // ------------------------------------------------------------------
    if (name == "findAsync") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "findAsync"), 2,
            [self](Runtime &rt, const Value &, const Value *args, size_t count) -> Value {
                if (count < 1) throw JSError(rt, "findAsync requires at least 1 argument");
                auto col        = args[0].getString(rt).utf8(rt);
                auto filterJson = count > 1 ? valueToFilterJson(rt, args[1]) : std::string("{}");
                TalaDbJob *job = taladb_find_start(self->db_, col.c_str(), filterJson.c_str());
                return awaitJobAsPromise(rt, job, /*parseAsJson=*/true);
            });
    }

    // ------------------------------------------------------------------
    // compact(): void
    // ------------------------------------------------------------------
    if (name == "compact") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "compact"), 0,
            [self](Runtime &rt, const Value &, const Value *, size_t) -> Value {
                int32_t res = taladb_compact(self->db_);
                if (res < 0) throw JSError(rt, "taladb_compact failed");
                return Value::undefined();
            });
    }

    // ------------------------------------------------------------------
    // close(): void  (synchronous — the destructor does the real work)
    // ------------------------------------------------------------------
    if (name == "close") {
        return Function::createFromHostFunction(
            rt, PropNameID::forAscii(rt, "close"), 0,
            [self](Runtime &, const Value &, const Value *, size_t) -> Value {
                if (self->db_) {
                    taladb_close(self->db_);
                    self->db_ = nullptr;
                }
                return Value::undefined();
            });
    }

    return Value::undefined();
}

} // namespace taladb
