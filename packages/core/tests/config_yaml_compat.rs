//! Configuration behavior recorded with serde_yaml_ng 0.10.0 before replacing
//! its unmaintained low-level parser. Exercise the public loader, not a parser
//! API, so these fixtures remain useful across dependency updates.

use taladb::config::{TalaDbConfig, load_from_path};

fn load(content: &str, extension: &str) -> Result<TalaDbConfig, taladb::TalaDbError> {
    let file = tempfile::Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile()
        .unwrap();
    std::fs::write(file.path(), content).unwrap();
    load_from_path(file.path())
}

#[cfg(feature = "config-yaml")]
#[test]
fn yaml_syntax_keeps_the_same_config_values_as_json() {
    let expected = load(
        r#"{"durability":{"flush_every_write":false,"flush_ms":250},"vector_cache_bytes":0,"vector_cache_memory_bytes":8589934592}"#,
        "json",
    )
    .unwrap();
    for yaml in [
        "durability:\n  flush_every_write: false\n  flush_ms: 250\nvector_cache_bytes: 0\nvector_cache_memory_bytes: 8589934592\n",
        "---\ndurability: {flush_every_write: FALSE, flush_ms: 0xFA}\nvector_cache_bytes: 0\nvector_cache_memory_bytes: 8589934592\n...\n",
        "base: &base {flush_every_write: false, flush_ms: 250}\ndurability: *base\nvector_cache_bytes: 0\nvector_cache_memory_bytes: 8589934592\n",
        "delay: &delay 250\ndurability: {flush_every_write: false, flush_ms: *delay}\nvector_cache_bytes: 0\nvector_cache_memory_bytes: 8589934592\n",
        "durability: !!map {flush_every_write: !!bool false, flush_ms: !!int 250}\nvector_cache_bytes: 0\nvector_cache_memory_bytes: 8589934592\n",
        "durability:\r\n  flush_every_write: false # comment\r\n  flush_ms: 250\r\nvector_cache_bytes: 0\r\nvector_cache_memory_bytes: 8589934592\r\n",
    ] {
        for extension in ["yml", "yaml"] {
            let actual = load(yaml, extension).unwrap_or_else(|error| panic!("{yaml:?}: {error}"));
            assert_eq!(actual, expected, "{yaml:?}");
        }
    }
}

#[cfg(feature = "config-yaml")]
#[test]
fn yaml_defaults_null_options_and_unknown_fields_are_preserved() {
    for yaml in [
        "{}",
        "durability: {}\n",
        "vector_cache_bytes: null\nvector_cache_memory_bytes: ~\ndurability: {flush_ms: null}\n",
        "webhook: {enabled: true, endpoint: 'https://example.invalid/日本語'}\nunknown: |\n  ignored 🚀\n  second line\ndurability: {unknown: [1, 2]}\n",
    ] {
        assert_eq!(load(yaml, "yml").unwrap(), TalaDbConfig::default());
    }

    // The loader has never expanded YAML merge keys. Starting to honor this
    // key would silently change durability for an existing configuration.
    let config = load(
        "base: &base {flush_every_write: false}\ndurability: {<<: *base, flush_ms: 50}\n",
        "yaml",
    )
    .unwrap();
    assert!(config.durability.flush_every_write);
    assert_eq!(config.durability.flush_ms, Some(50));
}

#[cfg(feature = "config-yaml")]
#[test]
fn yaml_invalid_configs_keep_the_config_error_contract() {
    for yaml in [
        "durability: [",
        "durability: {flush_every_write: 'false'}",
        "durability: {flush_every_write: yes}",
        "durability: {flush_every_write: no}",
        "durability: {flush_every_write: null}",
        "durability: {flush_ms: -1}",
        "durability: {flush_ms: 4294967296}",
        "durability: {flush_ms: 1.5}",
        "durability: {flush_ms: '250'}",
        "durability: null",
        "durability: {}\ndurability: {}",
        "durability: {flush_ms: 1, flush_ms: 2}",
        "vector_cache_memory_bytes: 0",
        "vector_cache_memory_bytes: -1",
        "vector_cache_memory_bytes: 18446744073709551616",
        "vector_cache_memory_bytes: true",
        "vector_cache_memory_bytes: 1.5",
        "vector_cache_memory_bytes: 8_589_934_592",
        "\u{feff}durability:\r\n  flush_every_write: false # comment\r\n  flush_ms: 250\r\nvector_cache_bytes: 0\r\nvector_cache_memory_bytes: 8589934592\r\n",
        "durability: *missing",
        "---\n{}\n---\n{}",
        "[1, 2]",
    ] {
        let error = load(yaml, "yaml").unwrap_err();
        match error {
            taladb::TalaDbError::Config(message) => {
                assert!(message.starts_with("invalid YAML config:"), "{message}");
            }
            other => panic!("wrong error for {yaml:?}: {other}"),
        }
    }
}

#[cfg(feature = "config-yaml")]
#[test]
fn yaml_discovery_keeps_extension_priority_and_does_not_hide_parse_errors() {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("taladb.config.yml", "durability: {flush_ms: 10}"),
        ("taladb.config.yaml", "durability: {flush_ms: 20}"),
        ("taladb.config.json", r#"{"durability":{"flush_ms":30}}"#),
    ] {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    assert_eq!(
        taladb::config::load_auto(dir.path())
            .unwrap()
            .durability
            .flush_ms,
        Some(10)
    );
    std::fs::remove_file(dir.path().join("taladb.config.yml")).unwrap();
    assert_eq!(
        taladb::config::load_auto(dir.path())
            .unwrap()
            .durability
            .flush_ms,
        Some(20)
    );
    std::fs::write(dir.path().join("taladb.config.yaml"), "durability: [").unwrap();
    assert!(matches!(
        taladb::config::load_auto(dir.path()),
        Err(taladb::TalaDbError::Config(_))
    ));
}

#[cfg(not(feature = "config-yaml"))]
#[test]
fn json_still_works_and_yaml_stays_disabled_without_the_feature() {
    assert_eq!(load("{}", "json").unwrap(), TalaDbConfig::default());
    match load("{}", "yaml").unwrap_err() {
        taladb::TalaDbError::Config(message) => {
            assert!(message.contains("YAML config support is not compiled"));
        }
        other => panic!("wrong error: {other}"),
    }
}
