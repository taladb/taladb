/**
 * TalaDB React Native — iOS TurboModule + JSI HostObject installer.
 *
 * Build setup (Xcode / CocoaPods)
 * --------------------------------
 *  1. Run `scripts/build-ios.sh` (or the release CI) to produce
 *     `ios/TalaDBFfi.xcframework` — device (arm64) + simulator (arm64 + x86_64).
 *  2. The podspec declares `vendored_frameworks` pointing to that xcframework.
 *     Headers and link flags are handled automatically by CocoaPods.
 *
 * Runtime flow
 * ------------
 *  `initialize(dbName)` opens the Rust database on the JS thread and installs
 *  `global.__TalaDB__` as a JSI HostObject.
 *
 *  How the module reaches the JS runtime depends on the architecture:
 *   - New Architecture (always on from React Native 0.82, where the legacy
 *     bridge is gone): the TurboModule manager hands the runtime and its
 *     CallInvoker to `installJSIBindingsWithRuntime:callInvoker:`. It only
 *     does so for modules that implement `getTurboModule:`, so that is
 *     implemented here with the Codegen spec.
 *   - Legacy bridge (RN < 0.82 with the New Architecture disabled):
 *     `RCTCxxBridge` exposes `runtime` and `jsCallInvoker`.
 *
 *  NativeTalaDB.ts routes all synchronous CRUD calls directly through
 *  `global.__TalaDB__` — the TurboModule stubs below satisfy Codegen but
 *  are never invoked at runtime.
 */

#import <React/RCTBridgeModule.h>
#import <ReactCommon/RCTTurboModule.h>
#import <ReactCommon/CallInvoker.h>
#import <jsi/jsi.h>

#if defined(RCT_NEW_ARCH_ENABLED) && __has_include(<ReactCommon/RCTTurboModuleWithJSIBindings.h>)
#define TALADB_USE_JSI_BINDINGS 1
#import <ReactCommon/RCTTurboModuleWithJSIBindings.h>
#import <TalaDB/TalaDB.h> // Codegen: NativeTalaDBSpecJSI
#else
#define TALADB_USE_JSI_BINDINGS 0
#import <React/RCTBridge+Private.h>
#endif

#include "../cpp/TalaDBHostObject.h"
#include "../cpp/taladb.h"

#import <Foundation/Foundation.h>

#include <functional>
#include <memory>

using namespace facebook::jsi;

// ---------------------------------------------------------------------------
// TalaDB — Obj-C TurboModule
// ---------------------------------------------------------------------------

#if TALADB_USE_JSI_BINDINGS
@interface TalaDB : NSObject <RCTBridgeModule, RCTTurboModule, RCTTurboModuleWithJSIBindings>
#else
@interface TalaDB : NSObject <RCTBridgeModule, RCTTurboModule>
#endif
@end

@implementation TalaDB {
#if TALADB_USE_JSI_BINDINGS
    // Set by installJSIBindingsWithRuntime:callInvoker: before any JS call can
    // reach this module. Both belong to the React instance that owns this
    // module and are torn down with it on reload.
    facebook::jsi::Runtime *_runtime;
    std::shared_ptr<facebook::react::CallInvoker> _callInvoker;
#endif
}

RCT_EXPORT_MODULE(TalaDB)

#if TALADB_USE_JSI_BINDINGS
- (std::shared_ptr<facebook::react::TurboModule>)getTurboModule:
    (const facebook::react::ObjCTurboModule::InitParams &)params {
    return std::make_shared<facebook::react::NativeTalaDBSpecJSI>(params);
}

- (void)installJSIBindingsWithRuntime:(facebook::jsi::Runtime &)runtime
                          callInvoker:(const std::shared_ptr<facebook::react::CallInvoker> &)callInvoker {
    _runtime = &runtime;
    _callInvoker = callInvoker;
}
#endif

// Runs `work` on the JS thread with the live runtime. Returns NO when no
// runtime is reachable yet, so the caller can reject its promise.
- (BOOL)runOnJSThread:(std::function<void(Runtime &)>)work {
#if TALADB_USE_JSI_BINDINGS
    Runtime *runtime = _runtime;
    if (!runtime || !_callInvoker) {
        return NO;
    }
    _callInvoker->invokeAsync([runtime, work = std::move(work)]() { work(*runtime); });
    return YES;
#else
    RCTCxxBridge *bridge = (RCTCxxBridge *)[RCTBridge currentBridge];
    if (!bridge || !bridge.runtime) {
        return NO;
    }
    bridge.jsCallInvoker->invokeAsync([bridge, work = std::move(work)]() {
        work(*(Runtime *)bridge.runtime);
    });
    return YES;
#endif
}

// ---- Class method: open DB and install the JSI HostObject ----------------

+ (NSString * _Nullable)installInRuntime:(facebook::jsi::Runtime &)rt
                  dbPath:(NSString *)path
              configJson:(NSString * _Nullable)configJson {
    // Close any previous database before reopening the same redb file.
    taladb::TalaDBHostObject::uninstall(rt);
    TalaDbHandle *handle = nullptr;
    if (configJson != nil) {
        handle = taladb_open_with_config(path.UTF8String, configJson.UTF8String);
    } else {
        handle = taladb_open(path.UTF8String);
    }

    if (!handle) {
        const char *raw = taladb_last_error();
        return raw ? [NSString stringWithUTF8String:raw] : @"failed to open TalaDB";
    }

    taladb::TalaDBHostObject::install(rt, handle);
    NSLog(@"[TalaDB] Installed JSI HostObject — db: %@", path);
    return nil;
}

// ---- initialize(dbName) → Promise<void>  ---------------------------------

RCT_EXPORT_METHOD(initialize:(NSString *)dbName
                  configJson:(NSString * _Nullable)configJson
                  resolve:(RCTPromiseResolveBlock)resolve
                  reject:(RCTPromiseRejectBlock)reject) {
    @try {
        NSString *support = [NSSearchPathForDirectoriesInDomains(
            NSApplicationSupportDirectory, NSUserDomainMask, YES) firstObject];
        [[NSFileManager defaultManager] createDirectoryAtPath:support
                                  withIntermediateDirectories:YES
                                                   attributes:@{NSFileProtectionKey: NSFileProtectionCompleteUntilFirstUserAuthentication}
                                                        error:nil];
        NSString *dbPath = [support stringByAppendingPathComponent:dbName];
        NSString *documents = [NSSearchPathForDirectoriesInDomains(
            NSDocumentDirectory, NSUserDomainMask, YES) firstObject];
        NSString *legacyPath = [documents stringByAppendingPathComponent:dbName];
        if (![[NSFileManager defaultManager] fileExistsAtPath:dbPath] &&
            [[NSFileManager defaultManager] fileExistsAtPath:legacyPath]) {
            NSError *migrationError = nil;
            if (![[NSFileManager defaultManager] moveItemAtPath:legacyPath toPath:dbPath error:&migrationError]) {
                reject(@"TALADB_MIGRATION_ERROR", migrationError.localizedDescription, migrationError);
                return;
            }
            NSString *legacySalt = [legacyPath stringByAppendingString:@".taladb-salt"];
            NSString *newSalt = [dbPath stringByAppendingString:@".taladb-salt"];
            if ([[NSFileManager defaultManager] fileExistsAtPath:legacySalt]) {
                [[NSFileManager defaultManager] moveItemAtPath:legacySalt toPath:newSalt error:nil];
            }
        }

        // Install the HostObject on the JS thread
        BOOL scheduled = [self runOnJSThread:[dbPath, configJson, resolve, reject](Runtime &rt) {
            NSString *error = [TalaDB installInRuntime:rt dbPath:dbPath configJson:configJson];
            if (error) reject(@"TALADB_INSTALL_ERROR", error, nil);
            else {
                NSDictionary *protection = @{NSFileProtectionKey: NSFileProtectionCompleteUntilFirstUserAuthentication};
                [[NSFileManager defaultManager] setAttributes:protection ofItemAtPath:dbPath error:nil];
                NSString *saltPath = [dbPath stringByAppendingString:@".taladb-salt"];
                [[NSFileManager defaultManager] setAttributes:protection ofItemAtPath:saltPath error:nil];
                NSURL *dbURL = [NSURL fileURLWithPath:dbPath];
                [dbURL setResourceValue:@YES forKey:NSURLIsExcludedFromBackupKey error:nil];
                NSURL *saltURL = [NSURL fileURLWithPath:saltPath];
                [saltURL setResourceValue:@YES forKey:NSURLIsExcludedFromBackupKey error:nil];
                resolve(nil);
            }
        }];
        if (!scheduled) {
            reject(@"TALADB_NO_BRIDGE", @"JSI runtime not available", nil);
        }
    } @catch (NSException *ex) {
        reject(@"TALADB_INIT_ERROR", ex.reason, nil);
    }
}

// ---- close() → Promise<void>  --------------------------------------------

RCT_EXPORT_METHOD(close:(RCTPromiseResolveBlock)resolve
                  reject:(RCTPromiseRejectBlock)reject) {
    BOOL scheduled = [self runOnJSThread:[resolve](Runtime &rt) {
        taladb::TalaDBHostObject::uninstall(rt);
        resolve(nil);
    }];
    if (!scheduled) {
        reject(@"TALADB_NO_BRIDGE", @"JSI runtime not available", nil);
    }
}

// ---- Synchronous stubs — all real work goes through the JSI HostObject ---
// These exist only to satisfy the TurboModule Codegen spec (NativeTalaDB.ts).

RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(NSString *, insert:(NSString *)collection doc:(NSDictionary *)doc) {
    return nil;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(NSArray *, insertMany:(NSString *)collection docs:(NSArray *)docs) {
    return nil;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(NSArray *, find:(NSString *)collection filter:(NSDictionary *)filter) {
    return nil;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(NSDictionary *, findOne:(NSString *)collection filter:(NSDictionary *)filter) {
    return nil;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(BOOL, updateOne:(NSString *)collection filter:(NSDictionary *)filter update:(NSDictionary *)update) {
    return NO;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(double, updateMany:(NSString *)collection filter:(NSDictionary *)filter update:(NSDictionary *)update) {
    return 0;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(BOOL, deleteOne:(NSString *)collection filter:(NSDictionary *)filter) {
    return NO;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(double, deleteMany:(NSString *)collection filter:(NSDictionary *)filter) {
    return 0;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(double, count:(NSString *)collection filter:(NSDictionary *)filter) {
    return 0;
}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, createIndex:(NSString *)collection field:(NSString *)field) {}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, dropIndex:(NSString *)collection field:(NSString *)field) {}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, createFtsIndex:(NSString *)collection field:(NSString *)field) {}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, dropFtsIndex:(NSString *)collection field:(NSString *)field) {}

// Vector index + findNearest — JSI-only; stubs exist to satisfy TurboModule Codegen.

RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, createVectorIndex:(NSString *)collection
                                                       field:(NSString *)field
                                                  dimensions:(double)dimensions
                                                        opts:(NSDictionary *)opts) {}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, dropVectorIndex:(NSString *)collection field:(NSString *)field) {}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(void, upgradeVectorIndex:(NSString *)collection field:(NSString *)field) {}
RCT_EXPORT_SYNCHRONOUS_TYPED_METHOD(NSArray *, findNearest:(NSString *)collection
                                                      field:(NSString *)field
                                                      query:(NSArray *)query
                                                       topK:(double)topK
                                                     filter:(NSDictionary *)filter) {
    return @[];
}

// Async variants use the standard Promise bridge — JSI-only at runtime.

RCT_EXPORT_METHOD(findNearestAsync:(NSString *)collection
                              field:(NSString *)field
                              query:(NSArray *)query
                               topK:(double)topK
                             filter:(NSDictionary *)filter
                            resolve:(RCTPromiseResolveBlock)resolve
                             reject:(RCTPromiseRejectBlock)reject) {
    resolve(@[]);
}

RCT_EXPORT_METHOD(findAsync:(NSString *)collection
                     filter:(NSDictionary *)filter
                    resolve:(RCTPromiseResolveBlock)resolve
                     reject:(RCTPromiseRejectBlock)reject) {
    resolve(@[]);
}

@end
