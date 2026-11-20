# RN JSI HostObject host harness (M5.1)

Runtime: hermes-ios 0.14.1 debug (Maven Central, sha1 450a62e73a8884823a4ccea10b9e59c079b82e28), macosx slice (x86_64+arm64).
FFI: `cargo build -p taladb-ffi` (debug) at dev ae525dc -> libtaladb_ffi.a.

    clang++ -std=c++17 -g -O1 -fexceptions -frtti -fno-omit-frame-pointer \
      -fsanitize=address,undefined -fno-sanitize=vptr -include cstdint -DOLD_GLUE=<0|1> \
      -I $HERMES/destroot/include -I <react-native>/cpp harness.cpp <react-native>/cpp/TalaDBHostObject.cpp \
      -F $HERMES/destroot/Library/Frameworks/macosx -framework hermesvm -Wl,-rpath,... \
      libtaladb_ffi.a -framework Security -framework CoreFoundation -framework SystemConfiguration -liconv
    ./harness smoke.js <smoke|k1|k2|deep> <tmpdir>

`-fno-sanitize=vptr`: UBSan's vptr check misfires on deleting HermesRuntimeImpl (hidden RTTI in the framework); harness-only.
old = dev ae525dc cpp/ with OLD_GLUE=1 (close = clear global, as TalaDBJni/TalaDB.mm on dev).
new = PR #142 cpp/ with OLD_GLUE=0 (TalaDBHostObject::uninstall).
Logs: <old|new>-<suite>.log. Not a device run: no iOS/Android, no RN bridge, no TalaDB.mm.
