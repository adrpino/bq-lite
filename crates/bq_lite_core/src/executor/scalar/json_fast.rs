use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::error::{BqError, BqResult};

use super::json_path::{parse_json_path_uncached, PathSeg};

const MAX_JSON_PATH_CACHE: usize = 128;

thread_local! {
    static STRICT_PATH_CACHE: RefCell<HashMap<String, Arc<[PathSeg]>>> =
        RefCell::new(HashMap::new());
    static LEGACY_PATH_CACHE: RefCell<HashMap<String, Arc<[PathSeg]>>> =
        RefCell::new(HashMap::new());
}

pub(super) fn parse_json_path_cached(
    path: &str,
    legacy_brackets: bool,
) -> BqResult<Arc<[PathSeg]>> {
    let cache = if legacy_brackets {
        &LEGACY_PATH_CACHE
    } else {
        &STRICT_PATH_CACHE
    };
    if let Some(segs) = cache.with(|c| c.borrow().get(path).cloned()) {
        return Ok(segs);
    }
    let parsed: Arc<[PathSeg]> = parse_json_path_uncached(path, legacy_brackets)?.into();
    cache.with(|c| {
        let mut map = c.borrow_mut();
        if map.len() >= MAX_JSON_PATH_CACHE {
            map.clear();
        }
        map.insert(path.to_owned(), Arc::clone(&parsed));
    });
    Ok(parsed)
}

/// Streaming zero-DOM scalar extractor for `JSON_VALUE` / `JSON_EXTRACT_SCALAR` on raw JSON
/// strings. Validates the full JSON document while skipping non-matching branches via
/// [`IgnoredAny`], avoiding `serde_json::Value` DOM allocations.
pub(super) fn extract_json_scalar_str(raw: &str, segs: &[PathSeg]) -> BqResult<Option<String>> {
    let mut de = serde_json::Deserializer::from_str(raw);
    let seed = PathSeed { segs };
    let val = seed
        .deserialize(&mut de)
        .map_err(|e| BqError::semantic(format!("Invalid JSON: {e}")))?;
    de.end()
        .map_err(|e| BqError::semantic(format!("Invalid JSON: {e}")))?;
    Ok(val)
}

struct PathSeed<'a> {
    segs: &'a [PathSeg],
}

impl<'de> DeserializeSeed<'de> for PathSeed<'_> {
    type Value = Option<String>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        let Some((first, rest)) = self.segs.split_first() else {
            return deserializer.deserialize_any(ScalarVisitor);
        };
        deserializer.deserialize_any(StepVisitor { first, rest })
    }
}

struct StepVisitor<'a> {
    first: &'a PathSeg,
    rest: &'a [PathSeg],
}

impl<'de> Visitor<'de> for StepVisitor<'_> {
    type Value = Option<String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let target_idx = match self.first {
            PathSeg::Idx(i) => Some(*i),
            PathSeg::Key(_) => None,
        };
        let mut idx = 0_usize;
        let mut found = None;
        loop {
            if target_idx == Some(idx) {
                let Some(v) = seq.next_element_seed(PathSeed { segs: self.rest })? else {
                    break;
                };
                found = v;
            } else if seq.next_element::<IgnoredAny>()?.is_none() {
                break;
            }
            idx += 1;
        }
        Ok(found)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let target_key = match self.first {
            PathSeg::Key(k) => Some(k.as_str()),
            PathSeg::Idx(_) => None,
        };
        let mut found = None;
        while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
            if target_key == Some(key.as_ref()) {
                found = map.next_value_seed(PathSeed { segs: self.rest })?;
            } else {
                let _ = map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }
}

struct ScalarVisitor;

impl<'de> Visitor<'de> for ScalarVisitor {
    type Value = Option<String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(Some(if v { "true" } else { "false" }.to_owned()))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(Some(v.to_string()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(Some(v.to_string()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        Ok(serde_json::Number::from_f64(v).map(|n| n.to_string()))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(Some(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(Some(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}
