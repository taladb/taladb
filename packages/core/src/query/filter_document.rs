//! Decode only the top-level fields used by a filter. Postcard is not
//! self-describing, so skipping a Value must still consume its variant and
//! recursively validate its payload rather than using IgnoredAny.
use crate::document::{Document, Value};
use crate::query::Filter;
use serde::de::{DeserializeSeed, EnumAccess, SeqAccess, VariantAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;
use ulid::Ulid;

/// The top-level field names to decode, sorted by length and then bytes. A
/// filter names a handful of fields, and every field of every scanned document
/// is looked up here: comparing a few short names is cheaper than hashing each
/// one, and most comparisons end at the length.
pub(crate) struct Fields<'a>(Vec<&'a str>);

impl<'a> Fields<'a> {
    pub(crate) fn of(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut names: Vec<&str> = names.into_iter().collect();
        names.sort_unstable_by(|a, b| by_length(a, b));
        names.dedup();
        Self(names)
    }
    fn contains(&self, name: &str) -> bool {
        self.0.binary_search_by(|f| by_length(f, name)).is_ok()
    }
}

fn by_length(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

pub(crate) fn fields(filter: &Filter) -> Fields<'_> {
    fn visit<'a>(filter: &'a Filter, out: &mut Vec<&'a str>) {
        match filter {
            Filter::All => {}
            Filter::And(children) | Filter::Or(children) => {
                for child in children {
                    visit(child, out);
                }
            }
            Filter::Not(child) => visit(child, out),
            Filter::Eq(f, _)
            | Filter::Ne(f, _)
            | Filter::Gt(f, _)
            | Filter::Gte(f, _)
            | Filter::Lt(f, _)
            | Filter::Lte(f, _)
            | Filter::In(f, _)
            | Filter::Nin(f, _)
            | Filter::Exists(f, _)
            | Filter::Contains(f, _)
            | Filter::Regex(f, _) => {
                out.push(f.split('.').next().unwrap_or(f));
            }
        }
    }
    let mut out = Vec::new();
    visit(filter, &mut out);
    Fields::of(out)
}

pub(crate) fn decode(bytes: &[u8], fields: &Fields<'_>) -> Result<Document, postcard::Error> {
    let mut de = postcard::Deserializer::from_bytes(bytes);
    DocumentSeed(fields).deserialize(&mut de)
}

/// About how many encoded bytes a projected document was decoded from: its id
/// and the fields it kept. Lengths and tags count as one byte and numbers as
/// nine, an upper bound, so the share of skipped bytes is never overstated.
pub(crate) fn decoded_len(doc: &Document) -> u64 {
    fn value(v: &Value) -> u64 {
        match v {
            Value::Null | Value::Bool(_) => 2,
            Value::Int(_) | Value::Float(_) => 9,
            Value::Str(s) => s.len() as u64 + 2,
            Value::Bytes(b) => b.len() as u64 + 2,
            Value::Array(items) => 2 + items.iter().map(value).sum::<u64>(),
            Value::Object(fields) => 2 + fields.iter().map(field).sum::<u64>(),
        }
    }
    fn field((name, v): &(String, Value)) -> u64 {
        name.len() as u64 + 1 + value(v)
    }
    // A ULID string and its length.
    27 + doc.fields.iter().map(field).sum::<u64>()
}

struct DocumentSeed<'a>(&'a Fields<'a>);
impl<'de> DeserializeSeed<'de> for DocumentSeed<'_> {
    type Value = Document;
    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Document, D::Error> {
        de.deserialize_tuple(2, self)
    }
}
impl<'de> Visitor<'de> for DocumentSeed<'_> {
    type Value = Document;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a document")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Document, A::Error> {
        // `Ulid`'s own impl reads the id into a `String` first; borrowing it
        // saves an allocation per document and fails on the same inputs.
        let id: &str = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
        let id = Ulid::from_string(id).map_err(serde::de::Error::custom)?;
        let fields = seq
            .next_element_seed(FieldsSeed(self.0))?
            .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
        Ok(Document { id, fields })
    }
}
struct FieldsSeed<'a>(&'a Fields<'a>);
impl<'de> DeserializeSeed<'de> for FieldsSeed<'_> {
    type Value = Vec<(String, Value)>;
    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for FieldsSeed<'_> {
    type Value = Vec<(String, Value)>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("document fields")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut fields = Vec::new();
        while let Some(field) = seq.next_element_seed(FieldSeed(self.0))? {
            if let Some(field) = field {
                fields.push(field);
            }
        }
        Ok(fields)
    }
}
struct FieldSeed<'a>(&'a Fields<'a>);
impl<'de> DeserializeSeed<'de> for FieldSeed<'_> {
    type Value = Option<(String, Value)>;
    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_tuple(2, self)
    }
}
impl<'de> Visitor<'de> for FieldSeed<'_> {
    type Value = Option<(String, Value)>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a field pair")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let name: &str = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
        if self.0.contains(name) {
            let value = seq
                .next_element()?
                .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
            Ok(Some((name.to_string(), value)))
        } else {
            seq.next_element::<SkipValue>()?
                .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
            Ok(None)
        }
    }
}

struct SkipValue;
impl<'de> Deserialize<'de> for SkipValue {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_enum("Value", &[], Self)
    }
}
impl<'de> Visitor<'de> for SkipValue {
    type Value = Self;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Value variant")
    }
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self, A::Error> {
        let (tag, variant) = data.variant::<u32>()?;
        match tag {
            0 => variant.unit_variant()?,
            1 => {
                variant.newtype_variant::<bool>()?;
            }
            2 => {
                variant.newtype_variant::<i64>()?;
            }
            3 => {
                variant.newtype_variant::<f64>()?;
            }
            4 => {
                variant.newtype_variant::<&str>()?;
            }
            5 => {
                variant.newtype_variant::<&[u8]>()?;
            }
            6 => {
                variant.newtype_variant::<SkipArray>()?;
            }
            7 => {
                variant.newtype_variant::<SkipObject>()?;
            }
            _ => return Err(serde::de::Error::custom("invalid Value variant")),
        }
        Ok(Self)
    }
}
struct SkipArray;
impl<'de> Deserialize<'de> for SkipArray {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_seq(Self)
    }
}
impl<'de> Visitor<'de> for SkipArray {
    type Value = Self;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self, A::Error> {
        while seq.next_element::<SkipValue>()?.is_some() {}
        Ok(Self)
    }
}
struct SkipObject;
impl<'de> Deserialize<'de> for SkipObject {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        de.deserialize_seq(Self)
    }
}
impl<'de> Visitor<'de> for SkipObject {
    type Value = Self;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self, A::Error> {
        while seq.next_element::<(&str, SkipValue)>()?.is_some() {}
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_skips_every_value_variant_and_preserves_nested_filter_semantics() {
        let doc = Document::new(vec![
            (
                "unused".into(),
                Value::Array(vec![
                    Value::Null,
                    Value::Bool(true),
                    Value::Int(-42),
                    Value::Float(1.5),
                    Value::Str("🚀".into()),
                    Value::Bytes(vec![0, 128, 255]),
                    Value::Object(vec![("nested".into(), Value::Array(vec![Value::Int(1)]))]),
                ]),
            ),
            (
                "meta".into(),
                Value::Object(vec![("tag".into(), Value::Str("yes".into()))]),
            ),
            (
                "embedding".into(),
                Value::Array(vec![Value::Float(0.5); 1536]),
            ),
        ]);
        let filter = Filter::Eq("meta.tag".into(), Value::Str("yes".into()));
        let bytes = postcard::to_allocvec(&doc).unwrap();
        let partial = decode(&bytes, &fields(&filter)).unwrap();
        assert_eq!(partial.id, doc.id);
        assert_eq!(partial.fields.len(), 1);
        assert!(filter.matches(&partial).unwrap());
        for length in [0, 1, bytes.len() - 1] {
            assert!(decode(&bytes[..length], &fields(&filter)).is_err());
        }
    }
}
