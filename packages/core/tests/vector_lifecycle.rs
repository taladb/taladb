use taladb_core::{
    Database, Filter, GraphOptions, HnswOptions, Quantization, Update, Value, VectorMetric,
    VectorQueryOptions, VectorSearchMode,
};

fn vector(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|x| Value::Float(f64::from(*x))).collect())
}
fn ann() -> VectorQueryOptions {
    VectorQueryOptions {
        mode: VectorSearchMode::Ann,
        ..Default::default()
    }
}
fn id_filter(id: ulid::Ulid) -> Filter {
    Filter::Eq("_id".into(), Value::Str(id.to_string()))
}

#[test]
fn graph_survives_reopen_and_updates_atomically_without_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vectors.db");
    let id;
    {
        let db = Database::open(&path).unwrap();
        let col = db.collection("docs").unwrap();
        col.create_vector_index(
            "v",
            2,
            None,
            Some(HnswOptions {
                m: 8,
                ef_construction: 64,
            }),
        )
        .unwrap();
        id = col.insert(vec![("v".into(), vector(&[1., 0.]))]).unwrap();
        assert_eq!(
            col.search_vectors("v", &[1., 0.], 1, None, &ann())
                .unwrap()
                .execution
                .path,
            "hnsw"
        );
    }
    let db = Database::open(&path).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(
        col.search_vectors("v", &[1., 0.], 1, None, &ann())
            .unwrap()
            .hits[0]
            .document
            .id,
        id
    );
    let before = col.vector_index_status("v").unwrap();
    col.update_one(
        id_filter(id),
        Update::Set(vec![("title".into(), Value::Str("new".into()))]),
    )
    .unwrap();
    assert_eq!(
        before.revision,
        col.vector_index_status("v").unwrap().revision
    );
    col.update_one(
        id_filter(id),
        Update::Set(vec![("v".into(), vector(&[0., 1.]))]),
    )
    .unwrap();
    let results = col
        .search_vectors("v", &[0., 1.], 10, None, &ann())
        .unwrap();
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].score, 1.0);
    let status = col.vector_index_status("v").unwrap();
    assert_eq!(status.state, "ready");
    assert_eq!(status.deleted_nodes, 1);
    assert_eq!(status.index_revision, Some(status.revision));
    assert!(
        col.insert_many(vec![
            vec![("v".into(), vector(&[1., 0.]))],
            vec![("v".into(), vector(&[1.]))]
        ])
        .is_err()
    );
    assert_eq!(col.vector_index_status("v").unwrap().indexed_vectors, 1);
    col.delete_one(id_filter(id)).unwrap();
    assert!(
        col.search_vectors("v", &[1., 0.], 10, None, &ann())
            .unwrap()
            .hits
            .is_empty()
    );
    col.upgrade_vector_index("v").unwrap();
    assert_eq!(col.vector_index_status("v").unwrap().deleted_nodes, 0);
}

#[cfg(feature = "encryption")]
#[test]
fn encrypted_graph_survives_reopen_and_updates_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encrypted-vectors.db");
    let first;
    {
        let db = Database::open_encrypted(&path, "vector graph test passphrase").unwrap();
        let col = db.collection("docs").unwrap();
        col.create_vector_index("v", 2, None, Some(HnswOptions::default()))
            .unwrap();
        first = col.insert(vec![("v".into(), vector(&[1., 0.]))]).unwrap();
    }
    let db = Database::open_encrypted(&path, "vector graph test passphrase").unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(
        col.search_vectors("v", &[1., 0.], 1, None, &ann())
            .unwrap()
            .hits[0]
            .document
            .id,
        first
    );
    let second = col.insert(vec![("v".into(), vector(&[0., 1.]))]).unwrap();
    assert_eq!(
        col.search_vectors("v", &[0., 1.], 1, None, &ann())
            .unwrap()
            .hits[0]
            .document
            .id,
        second
    );
}

#[test]
fn staged_build_resumes_after_reopen_cancels_and_rejects_concurrent_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("build.db");
    let build;
    {
        let db = Database::open(&path).unwrap();
        let col = db.collection("docs").unwrap();
        col.create_vector_index("v", 2, None, None).unwrap();
        for i in 0..24 {
            col.insert(vec![("v".into(), vector(&[1., i as f32]))])
                .unwrap();
        }
        build = col
            .begin_vector_build(
                "v",
                Some(GraphOptions {
                    m: 4,
                    ..Default::default()
                }),
            )
            .unwrap();
        assert_eq!(
            col.step_vector_build("v", &build.id, 8).unwrap().processed,
            8
        );
        assert_eq!(col.vector_index_status("v").unwrap().state, "flat");
    }
    let db = Database::open(&path).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(
        col.step_vector_build("v", &build.id, 8).unwrap().processed,
        16
    );
    assert_eq!(
        col.step_vector_build("v", &build.id, 8).unwrap().state,
        "ready"
    );
    let before = col.search_vectors("v", &[1., 3.], 4, None, &ann()).unwrap();
    let cancelled = col.begin_vector_build("v", None).unwrap();
    col.step_vector_build("v", &cancelled.id, 5).unwrap();
    assert!(col.cancel_vector_build("v", "wrong-id").is_err());
    assert_eq!(
        col.cancel_vector_build("v", &cancelled.id).unwrap().state,
        "cancelled"
    );
    assert_eq!(
        col.search_vectors("v", &[1., 3.], 4, None, &ann())
            .unwrap()
            .hits[0]
            .document
            .id,
        before.hits[0].document.id
    );
    let stale = col.begin_vector_build("v", None).unwrap();
    col.step_vector_build("v", &stale.id, 8).unwrap();
    col.insert(vec![("v".into(), vector(&[-1., 0.]))]).unwrap();
    assert_eq!(
        col.step_vector_build("v", &stale.id, 8).unwrap().state,
        "failed"
    );
    assert_eq!(col.vector_index_status("v").unwrap().indexed_vectors, 25);
    assert_eq!(col.vector_index_status("v").unwrap().state, "ready");
}

#[test]
fn filtered_ann_threshold_grouping_offset_and_exact_default() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_vector_index(
        "v",
        2,
        Some(VectorMetric::Euclidean),
        Some(HnswOptions::default()),
    )
    .unwrap();
    for i in 0..60 {
        col.insert(vec![
            ("v".into(), vector(&[i as f32, 0.])),
            ("parent".into(), Value::Int(i / 3)),
            ("keep".into(), Value::Bool(i % 9 == 0)),
        ])
        .unwrap();
    }
    let filter = Filter::Eq("keep".into(), Value::Bool(true));
    let exact = col
        .search_vectors(
            "v",
            &[0., 0.],
            6,
            Some(filter.clone()),
            &VectorQueryOptions::default(),
        )
        .unwrap();
    assert_eq!(exact.execution.path, "exact");
    let approximate = col
        .search_vectors(
            "v",
            &[0., 0.],
            6,
            Some(filter),
            &VectorQueryOptions {
                ef_search: Some(60),
                ..ann()
            },
        )
        .unwrap();
    assert_eq!(approximate.execution.path, "hnsw");
    assert_eq!(
        exact.hits.iter().map(|h| h.document.id).collect::<Vec<_>>(),
        approximate
            .hits
            .iter()
            .map(|h| h.document.id)
            .collect::<Vec<_>>()
    );
    let expanded = col
        .search_vectors(
            "v",
            &[0., 0.],
            6,
            Some(Filter::Eq("keep".into(), Value::Bool(true))),
            &VectorQueryOptions {
                ef_search: Some(2),
                ..ann()
            },
        )
        .unwrap();
    assert_eq!(expanded.hits.len(), 6);
    for mode in [VectorSearchMode::Exact, VectorSearchMode::Ann] {
        let opts = VectorQueryOptions {
            mode,
            group_by: Some("parent".into()),
            score_threshold: Some(0.05),
            ..Default::default()
        };
        let first = col.search_vectors("v", &[0., 0.], 3, None, &opts).unwrap();
        let next = col
            .search_vectors(
                "v",
                &[0., 0.],
                3,
                None,
                &VectorQueryOptions { offset: 3, ..opts },
            )
            .unwrap();
        assert_eq!(
            first
                .hits
                .iter()
                .map(|h| h.document.get("parent").unwrap().clone())
                .collect::<Vec<_>>(),
            vec![Value::Int(0), Value::Int(1), Value::Int(2)]
        );
        assert_eq!(next.hits[0].document.get("parent"), Some(&Value::Int(3)));
        assert!(next.hits.iter().all(|h| h.score >= 0.05));
    }
}

#[test]
fn quantized_ann_recall_and_scores_are_measured_against_exact() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    let mut seed = 1234567u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (f64::from(seed as u32) / f64::from(u32::MAX) * 2. - 1.) as f32
    };
    let vectors: Vec<Vec<f32>> = (0..240)
        .map(|_| (0..32).map(|_| next()).collect())
        .collect();
    col.create_vector_index("v", 32, None, None).unwrap();
    col.insert_many(
        vectors
            .iter()
            .map(|v| vec![("v".into(), vector(v))])
            .collect(),
    )
    .unwrap();
    for quantization in [
        Quantization::None,
        Quantization::Scalar,
        Quantization::Binary,
    ] {
        col.rebuild_vector_index(
            "v",
            Some(GraphOptions {
                m: 8,
                ef_construction: 64,
                quantization,
            }),
        )
        .unwrap();
        let mut hits = 0;
        for query in vectors.iter().step_by(24) {
            let truth = col
                .search_vectors(
                    "v",
                    query,
                    5,
                    None,
                    &VectorQueryOptions {
                        mode: VectorSearchMode::Exact,
                        ..Default::default()
                    },
                )
                .unwrap();
            let result = col
                .search_vectors(
                    "v",
                    query,
                    5,
                    None,
                    &VectorQueryOptions {
                        ef_search: Some(100),
                        ..ann()
                    },
                )
                .unwrap();
            assert_eq!(result.execution.path, "hnsw");
            for hit in &result.hits {
                if let Some(exact) = truth.hits.iter().find(|t| t.document.id == hit.document.id) {
                    assert_eq!(hit.score, exact.score);
                    hits += 1;
                }
            }
        }
        assert!(hits >= 45, "{quantization:?}: recall@5 was {hits}/50");
    }
}

#[test]
fn legacy_index_is_visible_and_upgrade_actually_promotes_flat() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_vector_index("v", 2, None, None).unwrap();
    col.insert(vec![("v".into(), vector(&[1., 0.]))]).unwrap();
    assert!(col.search_vectors("v", &[1., 0.], 1, None, &ann()).is_err());
    {
        let mut txn = db.backend().begin_write().unwrap();
        txn.put(
            taladb_core::vector::META_HNSW_TABLE,
            b"docs::v",
            &postcard::to_allocvec(&HnswOptions::default()).unwrap(),
        )
        .unwrap();
        txn.commit().unwrap();
    }
    assert_eq!(
        col.vector_index_status("v").unwrap().state,
        "rebuildRequired"
    );
    assert_eq!(
        col.search_vectors("v", &[1., 0.], 1, None, &VectorQueryOptions::default())
            .unwrap()
            .execution
            .reason,
        "legacyIndexRequiresRebuild"
    );
    col.upgrade_vector_index("v").unwrap();
    assert_eq!(
        col.search_vectors("v", &[1., 0.], 1, None, &ann())
            .unwrap()
            .execution
            .path,
        "hnsw"
    );
    col.drop_vector_index("v").unwrap();
    assert!(
        db.backend()
            .begin_read()
            .unwrap()
            .list_tables()
            .unwrap()
            .iter()
            .all(|t| !t.starts_with("hnsw::docs::v::"))
    );
}

#[test]
fn deferred_build_validates_before_creating_the_flat_index() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    assert!(
        col.vector_command(
            serde_json::json!({
                "op": "create",
                "field": "v",
                "dimensions": 2,
                "metric": "dot",
                "options": { "m": 8 },
                "deferBuild": true
            }),
            &|_| Ok(Filter::All),
            &|_| serde_json::Value::Null,
        )
        .is_err()
    );
    assert!(col.list_indexes().unwrap().vector.is_empty());
}
