// Host harness: runs the real TalaDBHostObject.cpp inside a real Hermes
// runtime (macOS slice of hermes-ios 0.14.1, the engine RN 0.83 ships), linked
// against the real libtaladb_ffi.a. Install/close mirror TalaDBJni.cpp and
// TalaDB.mm. OLD_GLUE=1 reproduces dev ae525dc (close = clear the global);
// otherwise TalaDBHostObject::uninstall is used, as in the fix.
#include <hermes/hermes.h>
#include <jsi/jsi.h>
#include <jsi/instrumentation.h>
#include <sys/stat.h>

#include <chrono>
#include <deque>
#include <thread>
#include <fstream>
#include <iostream>
#include <sstream>

#include "TalaDBHostObject.h"

using namespace facebook::jsi;

static std::deque<std::shared_ptr<Function>> timers;

static void installFns(Runtime &rt, facebook::hermes::HermesRuntime &hrt) {
    auto g = rt.global();
    g.setProperty(rt, "print", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "print"), 1,
        [](Runtime &rt, const Value &, const Value *a, size_t n) -> Value {
            for (size_t i = 0; i < n; i++) std::cout << (i ? " " : "") << a[i].toString(rt).utf8(rt);
            std::cout << std::endl;
            return Value::undefined();
        }));
    g.setProperty(rt, "setTimeout", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "setTimeout"), 2,
        [](Runtime &rt, const Value &, const Value *a, size_t) -> Value {
            timers.push_back(std::make_shared<Function>(a[0].getObject(rt).getFunction(rt)));
            return Value::undefined();
        }));
    g.setProperty(rt, "setImmediate", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "setImmediate"), 1,
        [](Runtime &rt, const Value &, const Value *a, size_t) -> Value {
            timers.push_back(std::make_shared<Function>(a[0].getObject(rt).getFunction(rt)));
            return Value::undefined();
        }));
    g.setProperty(rt, "__gc", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "__gc"), 0,
        [&hrt](Runtime &, const Value &, const Value *, size_t) -> Value {
            hrt.instrumentation().collectGarbage("harness");
            return Value::undefined();
        }));
    g.setProperty(rt, "__exists", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "__exists"), 1,
        [](Runtime &rt, const Value &, const Value *a, size_t) -> Value {
            struct stat st;
            return Value(stat(a[0].getString(rt).utf8(rt).c_str(), &st) == 0);
        }));
    // Same steps as TalaDBJni nativeInstall / TalaDB.mm installInRuntime.
    g.setProperty(rt, "__open", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "__open"), 2,
        [](Runtime &rt, const Value &, const Value *a, size_t n) -> Value {
#if OLD_GLUE
            rt.global().setProperty(rt, "__TalaDB__", Value::undefined());
#else
            taladb::TalaDBHostObject::uninstall(rt);
#endif
            auto path = a[0].getString(rt).utf8(rt);
            TalaDbHandle *db = (n > 1 && a[1].isString())
                ? taladb_open_with_config(path.c_str(), a[1].getString(rt).utf8(rt).c_str())
                : taladb_open(path.c_str());
            if (!db) {
                const char *e = taladb_last_error();
                throw JSError(rt, e ? e : "failed to open TalaDB");
            }
            taladb::TalaDBHostObject::install(rt, db);
            return Value::undefined();
        }));
    // Same steps as TalaDBModule.close (JNI nativeClose / TalaDB.mm close).
    g.setProperty(rt, "__moduleClose", Function::createFromHostFunction(rt, PropNameID::forAscii(rt, "__moduleClose"), 0,
        [](Runtime &rt, const Value &, const Value *, size_t) -> Value {
#if OLD_GLUE
            rt.global().setProperty(rt, "__TalaDB__", Value::undefined());
#else
            taladb::TalaDBHostObject::uninstall(rt);
#endif
            return Value::undefined();
        }));
}

int main(int argc, char **argv) {
    if (argc < 4) { std::cerr << "usage: harness <script.js> <suite> <tmpdir>\n"; return 2; }
    std::ifstream f(argv[1]);
    std::stringstream ss; ss << f.rdbuf();
    int failures = 0;
    {
        auto hrt = facebook::hermes::makeHermesRuntime(
            ::hermes::vm::RuntimeConfig::Builder().withMicrotaskQueue(true).build());
        Runtime &rt = *hrt;
        installFns(rt, *hrt);
        rt.global().setProperty(rt, "__suite", String::createFromUtf8(rt, argv[2]));
        rt.global().setProperty(rt, "__dir", String::createFromUtf8(rt, argv[3]));
#if OLD_GLUE
        rt.global().setProperty(rt, "__oldGlue", Value(true));
#else
        rt.global().setProperty(rt, "__oldGlue", Value(false));
#endif
        try {
            rt.evaluateJavaScript(std::make_shared<StringBuffer>(ss.str()), "smoke.js");
            auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(120);
            while (!timers.empty() && std::chrono::steady_clock::now() < deadline) {
                std::this_thread::sleep_for(std::chrono::milliseconds(1));
                auto t = timers.front(); timers.pop_front();
                t->call(rt);
                hrt->drainMicrotasks();
            }
            hrt->drainMicrotasks();
            auto fv = rt.global().getProperty(rt, "__failures");
            failures = fv.isNumber() ? (int)fv.getNumber() : 99;
            auto done = rt.global().getProperty(rt, "__done");
            if (!done.isBool() || !done.getBool()) { std::cout << "NOT DONE (pending promises)\n"; failures++; }
        } catch (const JSIException &e) {
            std::cout << "UNCAUGHT: " << e.what() << std::endl;
            failures = 98;
        }
        rt.global().setProperty(rt, "__TalaDB__", Value::undefined());
    }
    std::cout << "HARNESS EXIT failures=" << failures << std::endl;
    return failures ? 1 : 0;
}
