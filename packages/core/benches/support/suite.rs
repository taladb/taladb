//! One definition per workload, used two ways.
//!
//! In a normal `cargo bench` run every case is timed by Criterion. With
//! `TALADB_BENCH_FINGERPRINTS=<file>` the same closures run once each and their
//! results are hashed instead, so the regression gate can prove that baseline and
//! candidate computed the same answer before it compares how long they took.
//! Keeping both behind one call site is the point: a fingerprint taken from a
//! re-typed copy of a query would only prove that the copy agrees with itself.
//!
//! Hashes are FNV-1a over a canonical encoding (ULID bytes, `f32` score bits,
//! tagged values), never `std::hash`, whose output is not stable across builds.
//! Results whose order the engine does not define — `$group` output, the
//! intersection behind a two-sided range — are hashed as sets.

use criterion::measurement::WallTime;
use criterion::{BatchSize, BenchmarkGroup, BenchmarkId, Criterion, SamplingMode, Throughput};
use std::fmt::Display;
use std::time::Duration;
use taladb::fts::{HybridSearchResult, TextSearchResult};
use taladb::{Document, Ulid, Value, VectorSearchResult};

pub enum Suite<'c> {
    Bench(&'c mut Criterion),
    Fingerprint(Vec<Record>),
}

pub struct Record {
    pub id: String,
    pub kind: &'static str,
    pub n: u64,
    pub hash: u64,
}

impl Suite<'_> {
    /// `TALADB_BENCH_GROUPS=doc,fts` limits a run to those families (the first
    /// segment of a case id). Fixtures are seeded before Criterion's own filter
    /// applies, so this is what keeps a filtered run from paying for every setup.
    pub fn enabled(&self, family: &str) -> bool {
        std::env::var("TALADB_BENCH_GROUPS")
            .map(|groups| groups.split(',').any(|g| g.trim() == family))
            .unwrap_or(true)
    }

    pub fn group(&mut self, name: &str) -> Group<'_> {
        match self {
            Suite::Bench(c) => Group::Bench(c.benchmark_group(name)),
            Suite::Fingerprint(out) => Group::Fingerprint {
                name: name.to_string(),
                out,
            },
        }
    }

    pub fn write_fingerprints(self, path: &std::ffi::OsStr) {
        let Suite::Fingerprint(mut records) = self else {
            unreachable!("not a fingerprint run")
        };
        records.sort_by(|a, b| a.id.cmp(&b.id));
        let cases: serde_json::Map<_, _> = records
            .into_iter()
            .map(|r| {
                (
                    r.id,
                    serde_json::json!({ "kind": r.kind, "n": r.n, "hash": format!("{:016x}", r.hash) }),
                )
            })
            .collect();
        let json = serde_json::json!({ "schema": 1, "cases": cases });
        std::fs::write(path, format!("{json:#}\n")).expect("write fingerprints");
    }
}

/// Collection sizes for a parameterised group: `TALADB_BENCH_SIZES` if set,
/// otherwise `default`. Sizes absent from `default` are only run when listed
/// explicitly.
pub fn sizes(default: &[usize]) -> Vec<usize> {
    match std::env::var("TALADB_BENCH_SIZES") {
        Ok(list) => list
            .split(',')
            .map(|s| {
                s.trim()
                    .parse()
                    .expect("TALADB_BENCH_SIZES must list integers")
            })
            .collect(),
        Err(_) => default.to_vec(),
    }
}

pub enum Group<'a> {
    Bench(BenchmarkGroup<'a, WallTime>),
    Fingerprint {
        name: String,
        out: &'a mut Vec<Record>,
    },
}

impl Group<'_> {
    pub fn sample_size(&mut self, n: usize) -> &mut Self {
        if let Group::Bench(g) = self {
            g.sample_size(n);
        }
        self
    }

    pub fn flat(&mut self) -> &mut Self {
        if let Group::Bench(g) = self {
            g.sampling_mode(SamplingMode::Flat);
        }
        self
    }

    pub fn throughput(&mut self, t: Throughput) -> &mut Self {
        if let Group::Bench(g) = self {
            g.throughput(t);
        }
        self
    }

    /// Fixed timing for cases whose setup dwarfs the measured routine: with
    /// flat sampling this pins each sample to one iteration, so the wall time is
    /// `samples × (setup + routine)` instead of growing with the CLI budget.
    pub fn timing(&mut self, warm_up: Duration, measurement: Duration) -> &mut Self {
        if let Group::Bench(g) = self {
            g.warm_up_time(warm_up);
            g.measurement_time(measurement);
        }
        self
    }

    /// Case without a parameter: Criterion id `group/id`.
    pub fn run<R: Fingerprint>(&mut self, id: &str, f: impl FnMut() -> R) {
        self.case(BenchmarkId::from_parameter(id), id.to_string(), f);
    }

    /// Case with a parameter: Criterion id `group/id/param`.
    pub fn run_with<R: Fingerprint>(
        &mut self,
        id: &str,
        param: impl Display,
        f: impl FnMut() -> R,
    ) {
        self.case(BenchmarkId::new(id, &param), format!("{id}/{param}"), f);
    }

    /// Case named by its parameter alone: Criterion id `group/param`.
    pub fn run_param<R: Fingerprint>(&mut self, param: impl Display, f: impl FnMut() -> R) {
        self.case(BenchmarkId::from_parameter(&param), param.to_string(), f);
    }

    fn case<R: Fingerprint>(&mut self, bench: BenchmarkId, id: String, mut f: impl FnMut() -> R) {
        match self {
            Group::Bench(g) => {
                g.bench_with_input(bench, &(), |b, _| b.iter(&mut f));
            }
            Group::Fingerprint { name, out } => record(out, format!("{name}/{id}"), &f()),
        }
    }

    /// Case whose input is rebuilt outside the timed region. `param: None`
    /// gives the Criterion id `group/id`, `Some(p)` gives `group/id/p`.
    pub fn run_batched<I, R: Fingerprint>(
        &mut self,
        id: &str,
        param: Option<&dyn Display>,
        mut setup: impl FnMut() -> I,
        mut routine: impl FnMut(I) -> R,
        size: BatchSize,
    ) {
        let (bench, id) = match param {
            Some(p) => (BenchmarkId::new(id, p), format!("{id}/{p}")),
            None => (BenchmarkId::from_parameter(id), id.to_string()),
        };
        match self {
            Group::Bench(g) => {
                g.bench_with_input(bench, &(), |b, _| {
                    b.iter_batched(&mut setup, &mut routine, size)
                });
            }
            Group::Fingerprint { name, out } => {
                let input = setup();
                record(out, format!("{name}/{id}"), &routine(input))
            }
        }
    }

    pub fn finish(self) {
        if let Group::Bench(g) = self {
            g.finish();
        }
    }
}

fn record(out: &mut Vec<Record>, id: String, result: &impl Fingerprint) {
    let mut h = Fnv::new();
    let (kind, n) = result.fingerprint(&mut h);
    out.push(Record {
        id,
        kind,
        n,
        hash: h.0,
    });
}

pub struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    pub fn bytes(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
    pub fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn ulid(&mut self, id: Ulid) {
        self.bytes(&u128::from(id).to_be_bytes());
    }
    fn value(&mut self, v: &Value) {
        match v {
            Value::Null => self.bytes(&[0]),
            Value::Bool(b) => self.bytes(&[1, u8::from(*b)]),
            Value::Int(i) => {
                self.bytes(&[2]);
                self.u64(*i as u64);
            }
            Value::Float(f) => {
                self.bytes(&[3]);
                self.u64(f.to_bits());
            }
            Value::Str(s) => {
                self.bytes(&[4]);
                self.u64(s.len() as u64);
                self.bytes(s.as_bytes());
            }
            Value::Bytes(b) => {
                self.bytes(&[5]);
                self.u64(b.len() as u64);
                self.bytes(b);
            }
            Value::Array(items) => {
                self.bytes(&[6]);
                self.u64(items.len() as u64);
                items.iter().for_each(|item| self.value(item));
            }
            Value::Object(fields) => {
                self.bytes(&[7]);
                self.u64(fields.len() as u64);
                for (key, value) in fields {
                    self.bytes(key.as_bytes());
                    self.value(value);
                }
            }
        }
    }
    /// Order-independent: hash each element on its own, then the sorted hashes.
    fn set<T>(&mut self, items: &[T], mut one: impl FnMut(&mut Self, &T)) {
        let mut hashes: Vec<u64> = items
            .iter()
            .map(|item| {
                let mut h = Self::new();
                one(&mut h, item);
                h.0
            })
            .collect();
        hashes.sort_unstable();
        hashes.iter().for_each(|h| self.u64(*h));
    }
}

/// What a workload result contributes to its fingerprint. Returns the kind and
/// an element count, which makes a mismatch readable without the hash.
pub trait Fingerprint {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64);
}

/// Unsorted results whose order the engine does not promise.
pub struct Unordered(pub Vec<Document>);

/// Several unsorted result sets from one iteration, each hashed as a set.
pub struct UnorderedBatch(pub Vec<Vec<Document>>);

/// Results from a fixture without fixed IDs: the generated ULIDs differ per run,
/// so hash the set of one field's values instead.
pub struct FieldSet(pub &'static str, pub Vec<Document>);

/// Vector hits from a fixture without fixed IDs: the score bits, in rank order.
pub struct Scores(pub Vec<VectorSearchResult>);

/// Aggregation output: whole rows, as a set, ignoring the generated row IDs.
pub struct Rows(pub Vec<Document>);

impl Fingerprint for Vec<Document> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        self.iter().for_each(|d| h.ulid(d.id));
        ("ids", self.len() as u64)
    }
}

impl Fingerprint for Option<Document> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        self.iter().for_each(|d| h.ulid(d.id));
        ("ids", u64::from(self.is_some()))
    }
}

impl Fingerprint for Vec<Option<Document>> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        for d in self {
            match d {
                Some(d) => h.ulid(d.id),
                None => h.bytes(&[0]),
            }
        }
        ("ids", self.iter().flatten().count() as u64)
    }
}

impl Fingerprint for Unordered {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.set(&self.0, |h, d| h.ulid(d.id));
        ("id-set", self.0.len() as u64)
    }
}

impl Fingerprint for UnorderedBatch {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        for docs in &self.0 {
            let mut inner = Fnv::new();
            inner.set(docs, |h, d| h.ulid(d.id));
            h.u64(inner.0);
        }
        ("id-sets", self.0.iter().map(|d| d.len() as u64).sum())
    }
}

impl Fingerprint for Rows {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.set(&self.0, |h, d| {
            for (key, value) in &d.fields {
                if key != "_id" {
                    h.bytes(key.as_bytes());
                    h.value(value);
                }
            }
        });
        ("row-set", self.0.len() as u64)
    }
}

impl Fingerprint for FieldSet {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.set(&self.1, |h, d| match d.get(self.0) {
            Some(v) => h.value(v),
            None => h.bytes(&[0xff]),
        });
        ("field-set", self.1.len() as u64)
    }
}

impl Fingerprint for Scores {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        self.0
            .iter()
            .for_each(|r| h.u64(u64::from(r.score.to_bits())));
        ("scores", self.0.len() as u64)
    }
}

impl Fingerprint for Vec<TextSearchResult> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        for r in self {
            h.ulid(r.document.id);
            h.u64(u64::from(r.score.to_bits()));
        }
        ("ids+scores", self.len() as u64)
    }
}

impl Fingerprint for Vec<HybridSearchResult> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        for r in self {
            h.ulid(r.document.id);
            h.u64(u64::from(r.score.to_bits()));
        }
        ("ids+scores", self.len() as u64)
    }
}

impl Fingerprint for Vec<VectorSearchResult> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        for r in self {
            h.ulid(r.document.id);
            h.u64(u64::from(r.score.to_bits()));
        }
        ("ids+scores", self.len() as u64)
    }
}

/// `insert_many` returns generated IDs, which are random unless the documents
/// carry their own; only the count is stable.
impl Fingerprint for Vec<Ulid> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.u64(self.len() as u64);
        ("count", self.len() as u64)
    }
}

/// Snapshot bytes embed generated IDs; only the length is stable.
impl Fingerprint for Vec<u8> {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.u64(self.len() as u64);
        ("bytes", self.len() as u64)
    }
}

impl Fingerprint for u64 {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.u64(*self);
        ("count", *self)
    }
}

impl Fingerprint for bool {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.bytes(&[u8::from(*self)]);
        ("bool", u64::from(*self))
    }
}

impl Fingerprint for f32 {
    fn fingerprint(&self, h: &mut Fnv) -> (&'static str, u64) {
        h.u64(u64::from(self.to_bits()));
        ("score-bits", 1)
    }
}

/// Mutations with no result worth comparing (index create/drop cycles). The
/// record still proves the case ran.
impl Fingerprint for () {
    fn fingerprint(&self, _: &mut Fnv) -> (&'static str, u64) {
        ("unit", 0)
    }
}
