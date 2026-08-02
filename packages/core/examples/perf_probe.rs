//! Temporary perf probe (not part of the library). Delete after use.
use std::time::Instant;
use taladb_core::query::options::FindOptions;
use taladb_core::{Database, Filter, Value};

fn mk(i: usize) -> Vec<(String, Value)> {
    vec![
        ("name".into(), Value::Str(format!("user-{i}"))),
        ("status".into(), Value::Str("active".into())),
        ("age".into(), Value::Int((i % 90) as i64)),
        ("city".into(), Value::Str(format!("city-{}", i % 100))),
        ("bio".into(), Value::Str("lorem ipsum dolor sit amet consectetur".into())),
    ]
}

fn main() {
    let n: usize = 20_000;

    for idx_count in [0usize, 1, 3] {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("users").unwrap();
        for f in ["status", "age", "city"].iter().take(idx_count) {
            col.create_index(f).unwrap();
        }
        let items: Vec<_> = (0..n).map(mk).collect();
        let t = Instant::now();
        col.insert_many(items).unwrap();
        println!(
            "insert_many {n} docs, {idx_count} secondary idx (+_changed_at auto): {:?}",
            t.elapsed()
        );
    }

    let db = Database::open_in_memory().unwrap();
    let col = db.collection("users").unwrap();
    col.create_index("status").unwrap();
    col.insert_many((0..n).map(mk).collect()).unwrap();

    let t = Instant::now();
    let all = col.find(Filter::All).unwrap();
    println!("full scan       {} docs: {:?}", all.len(), t.elapsed());

    let t = Instant::now();
    let hits = col
        .find(Filter::Eq("status".into(), Value::Str("active".into())))
        .unwrap();
    println!("indexed find    {} docs: {:?}", hits.len(), t.elapsed());

    let t = Instant::now();
    let c = col.count(Filter::All).unwrap();
    println!("count(All)      {c} docs: {:?}", t.elapsed());

    let t = Instant::now();
    let c = col
        .count(Filter::Eq("status".into(), Value::Str("active".into())))
        .unwrap();
    println!("count(indexed eq) {c}: {:?}", t.elapsed());

    let t = Instant::now();
    let one = col
        .find_one(Filter::Eq("status".into(), Value::Str("active".into())))
        .unwrap();
    println!("find_one indexed (hit={}): {:?}", one.is_some(), t.elapsed());

    let t = Instant::now();
    let one = col.find_one(Filter::Eq("age".into(), Value::Int(42))).unwrap();
    println!("find_one unindexed (hit={}): {:?}", one.is_some(), t.elapsed());

    let t = Instant::now();
    let page = col
        .find_with_options(
            Filter::All,
            FindOptions { limit: Some(20), ..Default::default() },
        )
        .unwrap();
    println!("find limit 20 (no sort) -> {}: {:?}", page.len(), t.elapsed());

    // IndexAnd: two indexed equality conjuncts, one barely selective.
    let db2 = Database::open_in_memory().unwrap();
    let col2 = db2.collection("users").unwrap();
    col2.create_index("status").unwrap();
    col2.create_index("city").unwrap();
    col2.insert_many((0..n).map(mk).collect()).unwrap();
    let t = Instant::now();
    let hits = col2
        .find(Filter::And(vec![
            Filter::Eq("status".into(), Value::Str("active".into())),
            Filter::Eq("city".into(), Value::Str("city-7".into())),
        ]))
        .unwrap();
    println!("$and 2 indexed eq -> {} docs: {:?}", hits.len(), t.elapsed());

    // Cold start on a real file.
    let dir = std::env::temp_dir().join("taladb_perf_probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("probe.redb");
    {
        let db = Database::open(&path).unwrap();
        let col = db.collection("users").unwrap();
        col.create_index("status").unwrap();
        col.insert_many((0..n).map(mk).collect()).unwrap();
    }
    for label in ["cold open #1", "cold open #2"] {
        let t = Instant::now();
        let db = Database::open(&path).unwrap();
        let open = t.elapsed();
        let t2 = Instant::now();
        let col = db.collection("users").unwrap();
        let c = col.count(Filter::All).unwrap();
        let first = t2.elapsed();
        let t3 = Instant::now();
        let _ = col.find_one(Filter::Eq("status".into(), Value::Str("active".into()))).unwrap();
        println!(
            "{label}: open {open:?}, first count({c}) {first:?}, first find_one {:?}",
            t3.elapsed()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
