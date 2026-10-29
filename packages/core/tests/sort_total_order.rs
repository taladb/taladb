//! Sorting numbers must never panic and must give one well-defined order.
//!
//! The comparator used to call NaN equal to every number, and compared an Int
//! with a Float through `as f64`. Neither is transitive: `1 < 2`, yet both
//! "equal" NaN; `2^53 < 2^53 + 1`, yet both "equal" the float `2^53`. Since Rust
//! 1.81 the standard sort can detect that and panic ("user-provided comparison
//! function does not correctly implement a total order"); otherwise it returns
//! an order that depends on the input order. Both reproduced on v0.12.1.
//!
//! The order now: numbers by exact value (Int against Float included), `-0.0`
//! equal to `0.0` and `Int(0)`, every NaN after `+inf` and equal to every other
//! NaN; ties on `_id`. Each test checks against an oracle written here, not
//! against the engine, on indexed and unindexed collections, through `find`,
//! partial sorts, pages and aggregation.
use std::cmp::Ordering::{self, Equal, Greater, Less};

use taladb::aggregate::{Accumulator, GroupKey, Stage};
use taladb::document::{Document, Value};
use taladb::query::options::{FindOptions, SortDirection, SortSpec};
use taladb::{Collection, Database, Filter};
use ulid::Ulid;

const TWO_53: i64 = 1 << 53;
const TWO_63: f64 = 9_223_372_036_854_775_808.0;

fn nan_bits(bits: u64) -> f64 {
    let f = f64::from_bits(bits);
    assert!(f.is_nan());
    f
}

/// Positive and negative NaN, with the default and extreme payloads.
fn nans() -> [f64; 6] {
    [
        f64::NAN,
        -f64::NAN,
        nan_bits(0x7FF0_0000_0000_0001),
        nan_bits(0x7FFF_FFFF_FFFF_FFFF),
        nan_bits(0xFFF0_0000_0000_0001),
        nan_bits(0xFFFF_FFFF_FFFF_FFFF),
    ]
}

/// Deterministic caller-supplied id: sorts by `n`.
fn id(n: usize) -> Ulid {
    let mut b = [0u8; 16];
    b[0] = 0x01;
    b[12..16].copy_from_slice(&(n as u32).to_be_bytes());
    Ulid::from_bytes(b)
}

/// The order this suite expects, written independently of the engine.
fn oracle(a: &Value, b: &Value) -> Ordering {
    fn int_float(x: i64, y: f64) -> Ordering {
        if y.is_nan() || y >= TWO_63 {
            return Less;
        }
        if y < -TWO_63 {
            return Greater;
        }
        let floor = y.floor();
        match i128::from(x).cmp(&(floor as i128)) {
            Equal if y == floor => Equal,
            Equal => Less,
            ord => ord,
        }
    }
    let canon = |f: f64| if f == 0.0 { 0.0 } else { f };
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Int(x), Value::Float(y)) => int_float(*x, *y),
        (Value::Float(x), Value::Int(y)) => int_float(*y, *x).reverse(),
        (Value::Float(x), Value::Float(y)) => match (x.is_nan(), y.is_nan()) {
            (true, true) => Equal,
            (true, false) => Greater,
            (false, true) => Less,
            (false, false) => canon(*x).total_cmp(&canon(*y)),
        },
        _ => unreachable!("numeric fixtures only"),
    }
}

/// Ids of `rows` (`(id, value)`) in the order a sort on `x` must return,
/// missing values first as the engine orders them.
fn expected(rows: &[(Ulid, Option<Value>)], dir: SortDirection) -> Vec<Ulid> {
    let mut rows = rows.to_vec();
    rows.sort_by(|(ia, a), (ib, b)| {
        let ord = match (a, b) {
            (Some(a), Some(b)) => oracle(a, b),
            (None, Some(_)) => Less,
            (Some(_), None) => Greater,
            (None, None) => Equal,
        };
        let ord = if dir == SortDirection::Desc {
            ord.reverse()
        } else {
            ord
        };
        ord.then(ia.cmp(ib))
    });
    rows.into_iter().map(|(id, _)| id).collect()
}

struct Fixture {
    _db: Database,
    col: Collection,
    rows: Vec<(Ulid, Option<Value>)>,
}

/// `values[i]` under id `i` (`None` leaves `x` out), inserted in `order`.
fn fixture(values: &[Option<Value>], order: &[usize], indexed: bool) -> Fixture {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("c").unwrap();
    if indexed {
        col.create_index("x").unwrap();
    }
    let docs = order
        .iter()
        .map(|&i| {
            let mut fields = vec![
                ("_id".to_string(), Value::Str(id(i).to_string())),
                ("i".to_string(), Value::Int(i as i64)),
            ];
            if let Some(v) = &values[i] {
                fields.push(("x".to_string(), v.clone()));
            }
            fields
        })
        .collect();
    col.insert_many(docs).unwrap();
    let rows = values
        .iter()
        .enumerate()
        .map(|(i, v)| (id(i), v.clone()))
        .collect();
    Fixture { _db: db, col, rows }
}

fn ids(docs: &[Document]) -> Vec<Ulid> {
    docs.iter().map(|d| d.id).collect()
}

fn find(col: &Collection, dir: SortDirection, skip: u64, limit: Option<u64>) -> Vec<Ulid> {
    let sort = vec![SortSpec {
        field: "x".into(),
        direction: dir,
    }];
    ids(&col
        .find_with_options(
            Filter::All,
            FindOptions {
                sort,
                skip,
                limit,
                ..Default::default()
            },
        )
        .unwrap())
}

fn aggregate(col: &Collection, dir: SortDirection, skip: u64, limit: Option<u64>) -> Vec<Ulid> {
    let mut pipeline = vec![Stage::Sort(vec![SortSpec {
        field: "x".into(),
        direction: dir,
    }])];
    if skip > 0 {
        pipeline.push(Stage::Skip(skip));
    }
    if let Some(n) = limit {
        pipeline.push(Stage::Limit(n));
    }
    ids(&col.aggregate(pipeline).unwrap())
}

/// Full sorts, partial sorts, pages and aggregation, both directions, against
/// the oracle.
fn check_every_shape(f: &Fixture, label: &str) {
    let n = f.rows.len();
    for dir in [SortDirection::Asc, SortDirection::Desc] {
        let want = expected(&f.rows, dir.clone());
        assert_eq!(
            find(&f.col, dir.clone(), 0, None),
            want,
            "{label} {dir:?} full"
        );
        assert_eq!(
            aggregate(&f.col, dir.clone(), 0, None),
            want,
            "{label} {dir:?} $sort"
        );
        for (skip, limit) in [
            (0, 1),
            (0, 5),
            (3, 7),
            (n / 2, 4),
            (n.saturating_sub(2), 10),
        ] {
            let page: Vec<Ulid> = want.iter().skip(skip).take(limit).copied().collect();
            let (s, l) = (skip as u64, Some(limit as u64));
            assert_eq!(
                find(&f.col, dir.clone(), s, l),
                page,
                "{label} {dir:?} find {skip}+{limit}"
            );
            assert_eq!(
                aggregate(&f.col, dir.clone(), s, l),
                page,
                "{label} {dir:?} aggregate {skip}+{limit}"
            );
        }
    }
}

/// Mixed numbers and NaNs in a pattern known to make the old comparator panic
/// (from n = 22 with every third value NaN), at every size up to 160, plus
/// variants with both NaN signs, all payloads, infinities, zeros and Ints.
fn nan_fixtures() -> Vec<(String, Vec<Option<Value>>)> {
    let mut out = Vec::new();
    for n in (2..=160).step_by(3) {
        let plain: Vec<Option<Value>> = (0..n)
            .map(|i| {
                Some(Value::Float(if i % 3 == 0 {
                    f64::NAN
                } else {
                    f64::from((i * 7919) % 101)
                }))
            })
            .collect();
        out.push((format!("every-third-NaN n={n}"), plain));
    }
    let specials = [
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(0.0),
        Value::Float(-0.0),
        Value::Int(0),
        Value::Int(i64::MAX),
        Value::Int(i64::MIN),
        Value::Float(f64::MAX),
        Value::Float(-f64::MIN_POSITIVE),
        Value::Float(f64::from_bits(1)),
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..12 {
        let n = 20 + round * 13;
        let values = (0..n)
            .map(|_| {
                let r = next();
                Some(match r % 6 {
                    0 => Value::Float(nans()[(r >> 8) as usize % 6]),
                    1 => specials[(r >> 8) as usize % specials.len()].clone(),
                    2 => Value::Int((r >> 16) as i64 % 50 - 25),
                    3 => Value::Float(((r >> 16) % 100) as f64 / 4.0 - 12.5),
                    4 => Value::Float(f64::from_bits(r)),
                    _ => Value::Int(r as i64),
                })
            })
            .collect();
        out.push((format!("random mix {round}"), values));
    }
    out
}

#[test]
fn nan_mixed_with_numbers_sorts_without_panicking() {
    for (label, values) in nan_fixtures() {
        let order: Vec<usize> = (0..values.len()).collect();
        for indexed in [false, true] {
            let f = fixture(&values, &order, indexed);
            check_every_shape(&f, &format!("{label} indexed={indexed}"));
        }
    }
}

#[test]
fn every_nan_sorts_after_infinity_and_ties_on_id() {
    let mut values: Vec<Option<Value>> =
        nans().into_iter().map(|f| Some(Value::Float(f))).collect();
    values.push(Some(Value::Float(f64::INFINITY)));
    values.push(Some(Value::Int(i64::MAX)));
    values.push(Some(Value::Float(f64::NEG_INFINITY)));
    values.push(Some(Value::Str("s".into())));
    values.push(None);
    let order: Vec<usize> = (0..values.len()).rev().collect();
    let f = fixture(&values, &order, false);
    // Missing, -inf, i64::MAX, +inf, the six NaNs by id, then the string.
    let want: Vec<Ulid> = [10, 8, 7, 6, 0, 1, 2, 3, 4, 5, 9].map(id).to_vec();
    assert_eq!(find(&f.col, SortDirection::Asc, 0, None), want);
    // Descending reverses the values; ties stay on id ascending.
    let want: Vec<Ulid> = [9, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10].map(id).to_vec();
    assert_eq!(find(&f.col, SortDirection::Desc, 0, None), want);
}

#[test]
fn signed_zero_and_integer_zero_tie_on_id() {
    let values = vec![
        Some(Value::Float(0.0)),
        Some(Value::Int(0)),
        Some(Value::Float(-0.0)),
        Some(Value::Float(-1.0)),
        Some(Value::Float(-0.0)),
        Some(Value::Int(1)),
    ];
    for order in [
        vec![0, 1, 2, 3, 4, 5],
        vec![5, 4, 3, 2, 1, 0],
        vec![2, 4, 0, 5, 1, 3],
    ] {
        for indexed in [false, true] {
            let f = fixture(&values, &order, indexed);
            let asc = [3, 0, 1, 2, 4, 5].map(id).to_vec();
            assert_eq!(find(&f.col, SortDirection::Asc, 0, None), asc);
            let desc = [5, 0, 1, 2, 4, 3].map(id).to_vec();
            assert_eq!(find(&f.col, SortDirection::Desc, 0, None), desc);
            check_every_shape(&f, &format!("zeros {order:?} indexed={indexed}"));
        }
    }
}

#[test]
fn integers_beyond_f64_precision_sort_by_exact_value() {
    let values: Vec<Option<Value>> = vec![
        Value::Int(TWO_53 + 1),                    // 0
        Value::Float(TWO_53 as f64),               // 1
        Value::Int(TWO_53),                        // 2
        Value::Int(TWO_53 - 1),                    // 3
        Value::Float((TWO_53 + 2) as f64),         // 4
        Value::Int(TWO_53 + 2),                    // 5
        Value::Int(i64::MAX),                      // 6
        Value::Float(TWO_63),                      // 7: i64::MAX as f64
        Value::Float(9_223_372_036_854_774_784.0), // 8: largest f64 below 2^63
        Value::Int(i64::MAX - 1023),               // 9: 2^63 - 1024, == 8
        Value::Int(i64::MIN),                      // 10
        Value::Float(-TWO_63),                     // 11: == i64::MIN
        Value::Int(i64::MIN + 1),                  // 12
        Value::Float(-((TWO_53 + 1) as f64)),      // 13: rounds to -2^53
        Value::Int(-TWO_53 - 1),                   // 14
    ]
    .into_iter()
    .map(Some)
    .collect();
    // Exact values: i64::MIN = -2^63 (10, 11 tie on id) < i64::MIN + 1 (12)
    // < -2^53 - 1 (14) < -2^53 (13) < 2^53 - 1 (3) < 2^53 (1, 2 tie)
    // < 2^53 + 1 (0) < 2^53 + 2 (4, 5 tie) < 2^63 - 1024 (8, 9 tie)
    // < i64::MAX (6) < 2^63 (7).
    let want = [10, 11, 12, 14, 13, 3, 1, 2, 0, 4, 5, 8, 9, 6, 7]
        .map(id)
        .to_vec();
    for order in [
        (0..15).collect::<Vec<_>>(),
        (0..15).rev().collect(),
        vec![7, 3, 11, 0, 14, 5, 9, 1, 12, 6, 2, 10, 8, 4, 13],
    ] {
        for indexed in [false, true] {
            let f = fixture(&values, &order, indexed);
            assert_eq!(find(&f.col, SortDirection::Asc, 0, None), want, "{order:?}");
            check_every_shape(&f, &format!("precision {order:?} indexed={indexed}"));
        }
    }
}

#[test]
fn output_is_identical_across_runs_and_insertion_orders() {
    let (_, values) = nan_fixtures().pop().unwrap();
    let n = values.len();
    let orders: Vec<Vec<usize>> = vec![
        (0..n).collect(),
        (0..n).rev().collect(),
        (0..n).map(|i| (i * 37) % n).collect::<Vec<_>>(),
    ];
    let mut seen: Option<Vec<Vec<Ulid>>> = None;
    for order in &orders {
        let mut sorted = order.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != n {
            continue; // 37 shares a factor with n
        }
        let f = fixture(&values, order, false);
        for _ in 0..3 {
            let run = vec![
                find(&f.col, SortDirection::Asc, 0, None),
                find(&f.col, SortDirection::Desc, 0, None),
                find(&f.col, SortDirection::Asc, 5, Some(9)),
                aggregate(&f.col, SortDirection::Desc, 2, Some(11)),
            ];
            match &seen {
                Some(first) => assert_eq!(&run, first),
                None => seen = Some(run),
            }
        }
    }
}

#[test]
fn paging_through_a_nan_field_returns_each_document_once_in_order() {
    let (_, values) = nan_fixtures().pop().unwrap();
    let order: Vec<usize> = (0..values.len()).collect();
    for indexed in [false, true] {
        let f = fixture(&values, &order, indexed);
        for dir in [SortDirection::Asc, SortDirection::Desc] {
            let mut walked = Vec::new();
            let mut skip = 0;
            loop {
                let page = find(&f.col, dir.clone(), skip, Some(7));
                if page.is_empty() {
                    break;
                }
                skip += page.len() as u64;
                walked.extend(page);
            }
            assert_eq!(walked, expected(&f.rows, dir));
        }
    }
}

/// The index path still declines NaN and mixed fields; a field it does serve
/// returns what the scan path does.
#[test]
fn indexed_and_unindexed_collections_agree() {
    let fields: Vec<(&str, Vec<Option<Value>>)> = vec![
        (
            "ints",
            (0..90)
                .map(|i| Some(Value::Int((i * 31) % 17 - 8)))
                .collect(),
        ),
        (
            "floats",
            (0..90)
                .map(|i| Some(Value::Float(f64::from((i * 31) % 17) + 0.5)))
                .collect(),
        ),
        (
            "infinities",
            (0..90)
                .map(|i| {
                    Some(Value::Float(match i % 3 {
                        0 => f64::INFINITY,
                        1 => f64::NEG_INFINITY,
                        _ => f64::from(i),
                    }))
                })
                .collect(),
        ),
        ("nan", nan_fixtures().pop().unwrap().1),
    ];
    for (label, values) in fields {
        let order: Vec<usize> = (0..values.len()).collect();
        let scan = fixture(&values, &order, false);
        let indexed = fixture(&values, &order, true);
        for dir in [SortDirection::Asc, SortDirection::Desc] {
            for (skip, limit) in [(0, None), (0, Some(10)), (13, Some(10)), (85, Some(10))] {
                assert_eq!(
                    find(&indexed.col, dir.clone(), skip, limit),
                    find(&scan.col, dir.clone(), skip, limit),
                    "{label} {dir:?} {skip} {limit:?}"
                );
            }
        }
        check_every_shape(&indexed, label);
    }
}

fn group_min_max(col: &Collection) -> Vec<(Value, Value, Value)> {
    col.aggregate(vec![Stage::Group {
        key: GroupKey::Field("g".into()),
        accumulators: vec![
            ("min".into(), Accumulator::Min("x".into())),
            ("max".into(), Accumulator::Max("x".into())),
        ],
    }])
    .unwrap()
    .into_iter()
    .map(|d| {
        (
            d.get("_id").cloned().unwrap(),
            d.get("min").cloned().unwrap(),
            d.get("max").cloned().unwrap(),
        )
    })
    .collect()
}

/// `$min`/`$max` use the same comparator: NaN is the largest number, so `$max`
/// returns it whatever the document order and `$min` passes over it.
#[test]
fn group_min_and_max_use_the_same_order() {
    let groups: Vec<(i64, Vec<Value>)> = vec![
        (
            0,
            vec![
                Value::Float(f64::NAN),
                Value::Float(1.0),
                Value::Float(-3.0),
            ],
        ),
        (
            1,
            vec![
                Value::Float(1.0),
                Value::Float(-f64::NAN),
                Value::Float(-3.0),
            ],
        ),
        (2, vec![Value::Float(f64::NAN), Value::Float(-f64::NAN)]),
        (
            3,
            vec![
                Value::Float(TWO_53 as f64),
                Value::Int(TWO_53 + 1),
                Value::Int(TWO_53 - 1),
            ],
        ),
        (
            4,
            vec![Value::Float(f64::INFINITY), Value::Float(f64::NEG_INFINITY)],
        ),
    ];
    for reverse in [false, true] {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("g").unwrap();
        let mut docs = Vec::new();
        let mut n = 0;
        for (g, values) in &groups {
            for v in values {
                docs.push(vec![
                    ("_id".to_string(), Value::Str(id(n).to_string())),
                    ("g".to_string(), Value::Int(*g)),
                    ("x".to_string(), v.clone()),
                ]);
                n += 1;
            }
        }
        if reverse {
            // Ids carry the order the group stage reads, so reverse them.
            let total = docs.len();
            for (i, d) in docs.iter_mut().enumerate() {
                d[0].1 = Value::Str(id(total - i).to_string());
            }
        }
        col.insert_many(docs).unwrap();
        let out = group_min_max(&col);
        let get = |g: i64| out.iter().find(|(k, _, _)| k == &Value::Int(g)).unwrap();
        let is_nan = |v: &Value| matches!(v, Value::Float(f) if f.is_nan());
        assert_eq!(get(0).1, Value::Float(-3.0), "reverse={reverse}");
        assert!(is_nan(&get(0).2), "reverse={reverse}");
        assert_eq!(get(1).1, Value::Float(-3.0), "reverse={reverse}");
        assert!(is_nan(&get(1).2), "reverse={reverse}");
        assert!(is_nan(&get(2).1) && is_nan(&get(2).2), "reverse={reverse}");
        assert_eq!(get(3).1, Value::Int(TWO_53 - 1), "reverse={reverse}");
        assert_eq!(get(3).2, Value::Int(TWO_53 + 1), "reverse={reverse}");
        assert_eq!(
            get(4).1,
            Value::Float(f64::NEG_INFINITY),
            "reverse={reverse}"
        );
        assert_eq!(get(4).2, Value::Float(f64::INFINITY), "reverse={reverse}");
    }
}
