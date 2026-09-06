//! Operations executed by the bounded native job executor, off the JS thread.
use super::*;
use serde_json::{Value as Json, json};

fn text(args: &[Json], i: usize) -> Result<&str, String> {
    args.get(i)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("argument {i} must be a string"))
}
fn value(args: &[Json], i: usize) -> Result<&Json, String> {
    args.get(i).ok_or_else(|| format!("missing argument {i}"))
}
fn number(args: &[Json], i: usize) -> Result<usize, String> {
    args.get(i)
        .and_then(Json::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("argument {i} must be a nonnegative integer"))
}
fn filter(args: &[Json], i: usize) -> Result<Filter, String> {
    parse_filter(&args.get(i).unwrap_or(&Json::Null).to_string())
}

pub(super) fn execute(h: &TalaDbHandle, op: &str, args: &[Json]) -> Result<Json, String> {
    macro_rules! core {
        ($e:expr) => {
            $e.map_err(|e| e.to_string())?
        };
    }
    match op {
        "compact" => {
            core!(h.db.compact());
            return Ok(Json::Null);
        }
        "flush" => {
            core!(h.db.flush());
            return Ok(Json::Null);
        }
        "userVersion" => return Ok(json!(core!(h.db.user_version()))),
        "setUserVersion" => {
            let version = u32::try_from(number(args, 0)?).map_err(|e| e.to_string())?;
            core!(h.db.set_user_version(version));
            return Ok(Json::Null);
        }
        "listCollectionNames" => return Ok(json!(core!(h.db.list_collection_names()))),
        _ => {}
    }
    let col = core!(h.collection(text(args, 0)?));
    let result = match op {
        "insert" => {
            let fields =
                json_to_fields(&value(args, 1)?.to_string()).ok_or("insert expects an object")?;
            json!(core!(col.insert(fields)).to_string())
        }
        "insertMany" => {
            let docs = value(args, 1)?
                .as_array()
                .ok_or("insertMany expects an array")?;
            let fields = docs
                .iter()
                .map(|doc| json_to_fields(&doc.to_string()).ok_or("batch members must be objects"))
                .collect::<Result<Vec<_>, _>>()?;
            json!(
                core!(col.insert_many(fields))
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            )
        }
        "find" => json!(
            core!(col.find(filter(args, 1)?))
                .iter()
                .map(doc_to_json)
                .collect::<Vec<_>>()
        ),
        "findOne" => core!(col.find_one(filter(args, 1)?))
            .as_ref()
            .map(doc_to_json)
            .unwrap_or(Json::Null),
        "count" => json!(core!(col.count(filter(args, 1)?))),
        "updateOne" | "updateMany" => {
            let update = parse_update(&value(args, 2)?.to_string()).ok_or("invalid update")?;
            if op == "updateOne" {
                json!(core!(col.update_one(filter(args, 1)?, update)))
            } else {
                json!(core!(col.update_many(filter(args, 1)?, update)))
            }
        }
        "deleteOne" => json!(core!(col.delete_one(filter(args, 1)?))),
        "deleteMany" => json!(core!(col.delete_many(filter(args, 1)?))),
        "aggregate" => {
            let pipeline = taladb_core::aggregate::parse_pipeline(value(args, 1)?, &|v| {
                json_to_filter(v).ok_or_else(|| "invalid aggregate filter".into())
            })?;
            json!(
                core!(col.aggregate(pipeline))
                    .iter()
                    .map(doc_to_json)
                    .collect::<Vec<_>>()
            )
        }
        "listIndexes" => {
            let info = core!(col.list_indexes());
            json!({ "btree": info.btree, "fts": info.fts, "vector": info.vector })
        }
        "createIndex" => {
            core!(col.create_index(text(args, 1)?));
            Json::Null
        }
        "dropIndex" => {
            core!(col.drop_index(text(args, 1)?));
            Json::Null
        }
        "createFtsIndex" => {
            core!(col.create_fts_index(text(args, 1)?));
            Json::Null
        }
        "dropFtsIndex" => {
            core!(col.drop_fts_index(text(args, 1)?));
            Json::Null
        }
        "createCompoundIndex" | "dropCompoundIndex" => {
            let fields = value(args, 1)?
                .as_array()
                .ok_or("fields must be an array")?
                .iter()
                .map(|v| v.as_str().ok_or("field must be a string"))
                .collect::<Result<Vec<_>, _>>()?;
            if op == "createCompoundIndex" {
                core!(col.create_compound_index(&fields));
            } else {
                core!(col.drop_compound_index(&fields));
            }
            Json::Null
        }
        "createVectorIndex" => {
            let options = args.get(3).unwrap_or(&Json::Null);
            let metric = match options.get("metric").and_then(Json::as_str) {
                None | Some("cosine") => VectorMetric::Cosine,
                Some("dot") => VectorMetric::Dot,
                Some("euclidean") => VectorMetric::Euclidean,
                _ => return Err("invalid vector metric".into()),
            };
            let hnsw = options
                .get("hnsw")
                .map(|v| serde_json::from_value::<HnswOptions>(v.clone()))
                .transpose()
                .map_err(|e| e.to_string())?;
            core!(col.create_vector_index(text(args, 1)?, number(args, 2)?, Some(metric), hnsw));
            Json::Null
        }
        "dropVectorIndex" => {
            core!(col.drop_vector_index(text(args, 1)?));
            Json::Null
        }
        "upgradeVectorIndex" => {
            core!(col.upgrade_vector_index(text(args, 1)?));
            Json::Null
        }
        "searchText" => {
            let options = args.get(5);
            let rows = core!(col.search_text_with(
                text(args, 1)?,
                text(args, 2)?,
                number(args, 3)?,
                &bm25_from_options(options),
                Some(filter(args, 4)?)
            ));
            json!(
                rows.iter()
                    .map(|r| json!({"document": doc_to_json(&r.document), "score": r.score}))
                    .collect::<Vec<_>>()
            )
        }
        "hybridSearch" => {
            let vector: Vec<f32> =
                serde_json::from_value(value(args, 4)?.clone()).map_err(|e| e.to_string())?;
            let options = args.get(7);
            let rows = core!(
                col.hybrid_search(taladb_core::fts::HybridQuery {
                    text_field: text(args, 1)?,
                    text: text(args, 2)?,
                    vector_field: text(args, 3)?,
                    vector: &vector,
                    top_k: number(args, 5)?,
                    filter: Some(filter(args, 6)?),
                    bm25: bm25_from_options(options),
                    rrf: rrf_from_options(options),
                    candidates: options
                        .and_then(|o| o.get("candidates"))
                        .and_then(Json::as_u64)
                        .map(|n| usize::try_from(n).map_err(|e| e.to_string()))
                        .transpose()?,
                })
            );
            json!(
                rows.iter()
                    .map(
                        |r| json!({"document": doc_to_json(&r.document), "score": r.score,
                "textRank": r.text_rank, "vectorRank": r.vector_rank})
                    )
                    .collect::<Vec<_>>()
            )
        }
        _ => return Err(format!("unknown async database operation: {op}")),
    };
    Ok(result)
}
