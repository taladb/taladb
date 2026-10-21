use taladb::bm25::FtsStats;
use taladb::fts::{FTS_STATS_KEY, fts_len_table_name, fts_stats_table_name};
use taladb::{Database, Filter, Update, Value};

fn stats(db: &Database) -> FtsStats {
    let txn = db.backend().begin_read().unwrap();
    let bytes = txn
        .get(&fts_stats_table_name("docs", "body"), FTS_STATS_KEY)
        .unwrap()
        .unwrap();
    postcard::from_bytes(&bytes).unwrap()
}

#[test]
fn tokenless_documents_are_counted_updated_and_fully_deleted() {
    for text in ["", "!!!", "a x", "ordinary searchable text"] {
        // Exercise both backfill and incremental maintenance.
        for backfill in [false, true] {
            let db = Database::open_in_memory().unwrap();
            let col = db.collection("docs").unwrap();
            if !backfill {
                col.create_fts_index("body").unwrap();
            }
            let id = col
                .insert(vec![("body".into(), Value::Str(text.into()))])
                .unwrap();
            if backfill {
                col.create_fts_index("body").unwrap();
            }
            assert_eq!(stats(&db).doc_count, 1);
            col.update_one(
                Filter::All,
                Update::Set(vec![("body".into(), Value::Str("rust rust".into()))]),
            )
            .unwrap();
            assert_eq!(
                stats(&db),
                FtsStats {
                    doc_count: 1,
                    total_len: 2
                }
            );
            col.update_many(
                Filter::All,
                Update::Set(vec![("body".into(), Value::Str(text.into()))]),
            )
            .unwrap();
            assert_eq!(stats(&db).doc_count, 1);
            assert_eq!(col.delete_many(Filter::All).unwrap(), 1);
            assert_eq!(stats(&db), FtsStats::default());
            let txn = db.backend().begin_read().unwrap();
            assert!(
                txn.get(&fts_len_table_name("docs", "body"), &id.to_bytes())
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn malformed_existing_stats_fail_reads_and_roll_back_every_mutation() {
    for corrupt in [vec![], vec![0x80], vec![1, 1, 99]] {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("docs").unwrap().with_audit_log("test".into());
        col.create_fts_index("body").unwrap();
        col.insert(vec![("body".into(), Value::Str("searchable text".into()))])
            .unwrap();
        let table = fts_stats_table_name("docs", "body");
        let mut txn = db.backend().begin_write().unwrap();
        txn.put(&table, FTS_STATS_KEY, &corrupt).unwrap();
        txn.commit().unwrap();
        let before = db.export_snapshot().unwrap();
        let error = col.search_text("body", "searchable", 10).unwrap_err();
        assert!(error.to_string().contains("corrupt FTS statistics"));
        assert!(
            col.insert(vec![("body".into(), Value::Str("another text".into()))])
                .is_err()
        );
        assert!(
            col.update_many(
                Filter::All,
                Update::Set(vec![("body".into(), Value::Str("changed".into()))])
            )
            .is_err()
        );
        assert!(col.delete_one(Filter::All).is_err());
        assert!(col.delete_many(Filter::All).is_err());
        assert_eq!(
            db.export_snapshot().unwrap(),
            before,
            "documents, indexes, stats and audit must roll back"
        );
    }
}

#[test]
fn missing_stats_on_an_empty_index_remain_supported() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_fts_index("body").unwrap();
    let mut txn = db.backend().begin_write().unwrap();
    txn.delete(&fts_stats_table_name("docs", "body"), FTS_STATS_KEY)
        .unwrap();
    txn.commit().unwrap();
    assert!(col.search_text("body", "rust", 10).unwrap().is_empty());
    col.insert(vec![("body".into(), Value::Str("rust".into()))])
        .unwrap();
    assert_eq!(
        stats(&db),
        FtsStats {
            doc_count: 1,
            total_len: 1
        }
    );
}
