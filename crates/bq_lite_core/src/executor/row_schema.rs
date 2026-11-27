use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::error::{BqError, BqResult};
use crate::types::{BqType, BqValue};

const INDEX_THRESHOLD: usize = 6;
const AMBIGUOUS_SLOT: usize = usize::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColSlot {
    Col(usize),
    Pseudo(usize),
}

/// Shared column metadata stored once per [`RowSchema`] instead of per row.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ColMeta {
    pub qualifier: Option<Arc<str>>,
    pub name: Arc<str>,
    pub ty: BqType,
}

/// Owned column binding used when constructing or mutating ad-hoc rows.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RowBinding {
    pub qualifier: Option<Arc<str>>,
    pub name: Arc<str>,
    pub ty: BqType,
    pub value: BqValue,
}

/// Zero-copy borrowed view of a single column inside an [`super::row::EvalRow`].
#[derive(Debug, Clone, Copy)]
pub(super) struct BindingRef<'a> {
    pub qualifier: Option<&'a Arc<str>>,
    pub name: &'a Arc<str>,
    pub ty: &'a BqType,
    pub value: &'a BqValue,
}

impl BindingRef<'_> {
    #[must_use]
    pub(super) fn to_owned_binding(self) -> RowBinding {
        RowBinding {
            qualifier: self.qualifier.cloned(),
            name: Arc::clone(self.name),
            ty: self.ty.clone(),
            value: self.value.clone(),
        }
    }
}

/// Immutable column schema shared across rows via `Arc<RowSchema>`.
/// Schemas wider than [`INDEX_THRESHOLD`] pre-build case-folded hash indexes for O(1) lookup.
#[derive(Debug, Clone, Default)]
pub(super) struct RowSchema {
    pub cols: Vec<ColMeta>,
    pub pseudo_cols: Vec<ColMeta>,
    ident_index: Option<HashMap<Box<str>, usize>>,
    qual_index: Option<HashMap<Box<str>, ColSlot>>,
}

static EMPTY_SCHEMA: OnceLock<Arc<RowSchema>> = OnceLock::new();

#[must_use]
pub(super) fn empty_row_schema() -> Arc<RowSchema> {
    Arc::clone(EMPTY_SCHEMA.get_or_init(|| Arc::new(RowSchema::default())))
}

impl RowSchema {
    #[must_use]
    pub(super) fn new(cols: Vec<ColMeta>, pseudo_cols: Vec<ColMeta>) -> Self {
        let mut schema = Self {
            cols,
            pseudo_cols,
            ident_index: None,
            qual_index: None,
        };
        schema.rebuild_indexes();
        schema
    }

    #[must_use]
    pub(super) fn from_cols(cols: Vec<ColMeta>) -> Arc<Self> {
        if cols.is_empty() {
            return empty_row_schema();
        }
        Arc::new(Self::new(cols, Vec::new()))
    }

    #[must_use]
    pub(super) fn concat(left: &Self, right: &Self) -> Arc<Self> {
        let mut cols = Vec::with_capacity(left.cols.len() + right.cols.len());
        cols.extend(left.cols.iter().cloned());
        cols.extend(right.cols.iter().cloned());
        let mut pseudo_cols = Vec::with_capacity(left.pseudo_cols.len() + right.pseudo_cols.len());
        pseudo_cols.extend(left.pseudo_cols.iter().cloned());
        pseudo_cols.extend(right.pseudo_cols.iter().cloned());
        Arc::new(Self::new(cols, pseudo_cols))
    }

    pub(super) fn push_col(&mut self, meta: ColMeta) {
        self.cols.push(meta);
        self.rebuild_indexes();
    }

    pub(super) fn rebuild_indexes(&mut self) {
        if self.cols.len() + self.pseudo_cols.len() <= INDEX_THRESHOLD {
            self.ident_index = None;
            self.qual_index = None;
            return;
        }
        let mut ident_map = HashMap::with_capacity(self.cols.len());
        let mut qual_map = HashMap::with_capacity(self.cols.len() + self.pseudo_cols.len());
        for (idx, col) in self.cols.iter().enumerate() {
            let lower = col.name.to_ascii_lowercase();
            ident_map
                .entry(lower.clone().into_boxed_str())
                .and_modify(|slot| *slot = AMBIGUOUS_SLOT)
                .or_insert(idx);
            if let Some(ref q) = col.qualifier {
                let key = make_qual_key(q, &lower);
                qual_map.entry(key).or_insert(ColSlot::Col(idx));
            }
        }
        for (idx, col) in self.pseudo_cols.iter().enumerate() {
            if let Some(ref q) = col.qualifier {
                let lower = col.name.to_ascii_lowercase();
                let key = make_qual_key(q, &lower);
                qual_map.entry(key).or_insert(ColSlot::Pseudo(idx));
            }
        }
        self.ident_index = Some(ident_map);
        self.qual_index = Some(qual_map);
    }

    pub(super) fn lookup_ident_slot(&self, name: &str) -> BqResult<Option<ColSlot>> {
        if let Some(ref map) = self.ident_index {
            if let Some(&slot) = lookup_ascii_lower(map, name) {
                if slot == AMBIGUOUS_SLOT {
                    return Err(BqError::query(format!("Column name {name} is ambiguous")));
                }
                return Ok(Some(ColSlot::Col(slot)));
            }
        } else {
            let mut found: Option<usize> = None;
            for (idx, col) in self.cols.iter().enumerate() {
                if col.name.eq_ignore_ascii_case(name) {
                    if found.is_some() {
                        return Err(BqError::query(format!("Column name {name} is ambiguous")));
                    }
                    found = Some(idx);
                }
            }
            if let Some(idx) = found {
                return Ok(Some(ColSlot::Col(idx)));
            }
        }
        for (idx, col) in self.pseudo_cols.iter().enumerate() {
            if col.name.eq_ignore_ascii_case(name) {
                return Ok(Some(ColSlot::Pseudo(idx)));
            }
        }
        Ok(None)
    }

    pub(super) fn lookup_qualified_slot(&self, qualifier: &str, name: &str) -> Option<ColSlot> {
        if let Some(ref map) = self.qual_index {
            if let Some(slot) = lookup_qual_ascii_lower(map, qualifier, name) {
                return Some(slot);
            }
            if qualifier.is_ascii()
                && name.is_ascii()
                && !qualifier.contains('\0')
                && !name.contains('\0')
                && qualifier.len() + 1 + name.len() <= 128
            {
                return None;
            }
        }
        for (idx, col) in self.cols.iter().enumerate() {
            if col
                .qualifier
                .as_deref()
                .is_some_and(|q| q.eq_ignore_ascii_case(qualifier))
                && col.name.eq_ignore_ascii_case(name)
            {
                return Some(ColSlot::Col(idx));
            }
        }
        for (idx, col) in self.pseudo_cols.iter().enumerate() {
            if col
                .qualifier
                .as_deref()
                .is_some_and(|q| q.eq_ignore_ascii_case(qualifier))
                && col.name.eq_ignore_ascii_case(name)
            {
                return Some(ColSlot::Pseudo(idx));
            }
        }
        None
    }
}

fn make_qual_key(qualifier: &str, lower_name: &str) -> Box<str> {
    let mut s = String::with_capacity(qualifier.len() + 1 + lower_name.len());
    s.push_str(&qualifier.to_ascii_lowercase());
    s.push('\0');
    s.push_str(lower_name);
    s.into_boxed_str()
}

fn lookup_ascii_lower<'a, V>(map: &'a HashMap<Box<str>, V>, key: &str) -> Option<&'a V> {
    let mut buf = [0_u8; 64];
    if key.len() <= buf.len() && key.is_ascii() {
        for (dst, src) in buf.iter_mut().zip(key.as_bytes()) {
            *dst = src.to_ascii_lowercase();
        }
        if let Ok(lower) = std::str::from_utf8(buf.get(..key.len()).unwrap_or(&[])) {
            return map.get(lower);
        }
    }
    let owned = key.to_ascii_lowercase();
    map.get(owned.as_str())
}

fn lookup_qual_ascii_lower(
    map: &HashMap<Box<str>, ColSlot>,
    qualifier: &str,
    name: &str,
) -> Option<ColSlot> {
    let total = qualifier.len() + 1 + name.len();
    let mut buf = [0_u8; 128];
    if total <= buf.len() && qualifier.is_ascii() && name.is_ascii() {
        let mut pos = 0_usize;
        for &b in qualifier.as_bytes() {
            if let Some(slot) = buf.get_mut(pos) {
                *slot = b.to_ascii_lowercase();
            }
            pos += 1;
        }
        if let Some(slot) = buf.get_mut(pos) {
            *slot = 0;
        }
        pos += 1;
        for &b in name.as_bytes() {
            if let Some(slot) = buf.get_mut(pos) {
                *slot = b.to_ascii_lowercase();
            }
            pos += 1;
        }
        if let Ok(k) = std::str::from_utf8(buf.get(..total).unwrap_or(&[])) {
            return map.get(k).copied();
        }
    }
    let key = make_qual_key(qualifier, &name.to_ascii_lowercase());
    map.get(key.as_ref()).copied()
}
