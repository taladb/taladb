require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name          = "taladb-react-native"
  s.version       = package["version"]
  s.summary       = package["description"]
  s.homepage      = "https://github.com/taladb/taladb"
  s.license       = package["license"]
  s.authors       = { "taladb" => "hello@thinkgrid.io" }

  s.platforms     = { :ios => "13.0" }
  s.source        = { :git => "https://github.com/taladb/taladb.git",
                      :tag => "v#{s.version}" }

  # TypeScript / JS sources (not compiled by Xcode, just bundled)
  s.source_files  = "ios/**/*.{h,m,mm}", "cpp/**/*.{h,cpp}"
  # Android-only JNI glue: needs <jni.h>, which the iOS SDK does not have.
  s.exclude_files = "cpp/TalaDBJni.cpp"

  # ---------------------------------------------------------------------------
  # Pre-built Rust XCFramework
  # ---------------------------------------------------------------------------
  # Built by scripts/build-ios.sh (or the release CI):
  #   cargo build --target aarch64-apple-ios        --release  (device)
  #   cargo build --target aarch64-apple-ios-sim    --release  (Apple Silicon simulator)
  #   cargo build --target x86_64-apple-ios         --release  (Intel simulator)
  #   lipo sim slices → fat sim lib (named libtaladb_ffi.a, like the device
  #   slice — CocoaPods rejects xcframeworks whose slices differ in name)
  #   xcodebuild -create-xcframework → ios/TalaDBFfi.xcframework
  s.vendored_frameworks = "ios/TalaDBFfi.xcframework"

  # ---------------------------------------------------------------------------
  # Compiler settings
  # ---------------------------------------------------------------------------
  s.pod_target_xcconfig = {
    "CLANG_CXX_LANGUAGE_STANDARD" => "c++17",
    "OTHER_CPLUSPLUSFLAGS"        => "-DFOLLY_NO_CONFIG -DFOLLY_MOBILE=1 -DFOLLY_USE_LIBCPP=1",
    "HEADER_SEARCH_PATHS"         => "$(PODS_ROOT)/Headers/Public/React-Core $(PODS_ROOT)/Headers/Public/React-RCTFabric",
    "OTHER_LDFLAGS"               => "-lc++ -lz",
  }

  # ---------------------------------------------------------------------------
  # React Native dependencies
  # ---------------------------------------------------------------------------
  # install_modules_dependencies (RN >= 0.71) adds the React Native pods, the
  # Codegen header path (<TalaDB/TalaDB.h>) and -DRCT_NEW_ARCH_ENABLED when the
  # New Architecture is on — TalaDB.mm needs all three. It merges into the
  # pod_target_xcconfig above, so it must run after it.
  if respond_to?(:install_modules_dependencies, true)
    install_modules_dependencies(s)
  else
    s.dependency "React-Core"
    s.dependency "React-jsi"
    s.dependency "ReactCommon/turbomodule/core"
  end
end
