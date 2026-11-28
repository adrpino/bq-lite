use std::borrow::Cow;
use std::sync::Arc;

use crate::error::{BqError, BqResult};
use crate::types::{BqStructFieldValue, BqType, BqValue, StructField};

use super::result::QueryResult;
use super::row_schema::empty_row_schema;

pub(super) use super::row_schema::{BindingRef, ColMeta, ColSlot, RowBinding, RowSchema};

/// Evaluation row pairing a shared [`RowSchema`] with flat value vectors.
#[derive(Debug, Clone)]
pub(super) struct EvalRow {
    pub schema: Arc<RowSchema>,
    pub values: Vec<BqValue>,
    pub pseudo_values: Vec<BqValue>,
}

impl Default for EvalRow {
    fn default() -> Self {
        Self {
            schema: empty_row_schema(),
            values: Vec::new(),
            pseudo_values: Vec::new(),
        }
    }
}

impl EvalRow {
    #[must_use]
    pub(super) const fn from_schema(schema: Arc<RowSchema>, values: Vec<BqValue>) -> Self {
        Self {
            schema,
            values,
            pseudo_values: Vec::new(),
        }
    }

    #[must_use]
    pub(super) const fn from_schema_with_pseudo(
        schema: Arc<RowSchema>,
        values: Vec<BqValue>,
        pseudo_values: Vec<BqValue>,
    ) -> Self {
        Self {
            schema,
            values,
            pseudo_values,
        }
    }

    #[must_use]
    pub(super) fn new(cols: Vec<RowBinding>) -> Self {
        if cols.is_empty() {
            return Self::default();
        }
        let mut metas = Vec::with_capacity(cols.len());
        let mut values = Vec::with_capacity(cols.len());
        for b in cols {
            metas.push(ColMeta {
                qualifier: b.qualifier,
                name: b.name,
                ty: b.ty,
            });
            values.push(b.value);
        }
        Self::from_schema(RowSchema::from_cols(metas), values)
    }

    #[must_use]
    pub(super) fn with_pseudo(cols: Vec<RowBinding>, pseudo_cols: Vec<RowBinding>) -> Self {
        if cols.is_empty() && pseudo_cols.is_empty() {
            return Self::default();
        }
        let mut metas = Vec::with_capacity(cols.len());
        let mut values = Vec::with_capacity(cols.len());
        for b in cols {
            metas.push(ColMeta {
                qualifier: b.qualifier,
                name: b.name,
                ty: b.ty,
            });
            values.push(b.value);
        }
        let mut pmetas = Vec::with_capacity(pseudo_cols.len());
        let mut pvalues = Vec::with_capacity(pseudo_cols.len());
        for b in pseudo_cols {
            pmetas.push(ColMeta {
                qualifier: b.qualifier,
                name: b.name,
                ty: b.ty,
            });
            pvalues.push(b.value);
        }
        Self::from_schema_with_pseudo(Arc::new(RowSchema::new(metas, pmetas)), values, pvalues)
    }

    #[must_use]
    pub(super) fn from_query_result(alias: Option<&str>, qr: &QueryResult) -> Vec<Self> {
        let qual: Option<Arc<str>> = alias.map(Arc::from);
        let metas: Vec<ColMeta> = qr
            .columns
            .iter()
            .map(|c| ColMeta {
                qualifier: qual.clone(),
                name: Arc::from(c.name.as_str()),
                ty: c.ty.clone(),
            })
            .collect();
        let schema = RowSchema::from_cols(metas);
        qr.rows
            .iter()
            .map(|row| Self::from_schema(Arc::clone(&schema), row.clone()))
            .collect()
    }

    #[must_use]
    pub(super) fn combine_with_schema(left: &Self, right: &Self, schema: Arc<RowSchema>) -> Self {
        let mut values = Vec::with_capacity(left.values.len() + right.values.len());
        values.extend_from_slice(&left.values);
        values.extend_from_slice(&right.values);
        let pseudo_values = if left.pseudo_values.is_empty() && right.pseudo_values.is_empty() {
            Vec::new()
        } else {
            let mut pv = Vec::with_capacity(left.pseudo_values.len() + right.pseudo_values.len());
            pv.extend_from_slice(&left.pseudo_values);
            pv.extend_from_slice(&right.pseudo_values);
            pv
        };
        Self::from_schema_with_pseudo(schema, values, pseudo_values)
    }

    #[must_use]
    pub(super) fn col_len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub(super) fn col(&self, idx: usize) -> Option<BindingRef<'_>> {
        let meta = self.schema.cols.get(idx)?;
        let value = self.values.get(idx)?;
        Some(BindingRef {
            qualifier: meta.qualifier.as_ref(),
            name: &meta.name,
            ty: &meta.ty,
            value,
        })
    }

    pub(super) fn iter_cols(&self) -> impl ExactSizeIterator<Item = BindingRef<'_>> {
        self.schema
            .cols
            .iter()
            .zip(self.values.iter())
            .map(|(meta, value)| BindingRef {
                qualifier: meta.qualifier.as_ref(),
                name: &meta.name,
                ty: &meta.ty,
                value,
            })
    }

    pub(super) fn iter_pseudo(&self) -> impl ExactSizeIterator<Item = BindingRef<'_>> {
        self.schema
            .pseudo_cols
            .iter()
            .zip(self.pseudo_values.iter())
            .map(|(meta, value)| BindingRef {
                qualifier: meta.qualifier.as_ref(),
                name: &meta.name,
                ty: &meta.ty,
                value,
            })
    }

    #[must_use]
    pub(super) fn to_bindings(&self) -> Vec<RowBinding> {
        self.iter_cols().map(BindingRef::to_owned_binding).collect()
    }

    pub(super) fn push_binding(&mut self, binding: RowBinding) {
        Arc::make_mut(&mut self.schema).push_col(ColMeta {
            qualifier: binding.qualifier,
            name: binding.name,
            ty: binding.ty,
        });
        self.values.push(binding.value);
    }

    /// Appends a computed column across a batch of rows while sharing the extended [`RowSchema`]
    /// whenever consecutive rows have identical input schemas and column types.
    pub(super) fn append_column_batch(
        rows: &mut [Self],
        name: &Arc<str>,
        vals: Vec<BqValue>,
    ) -> BqResult<()> {
        let mut cached: Option<(Arc<RowSchema>, BqType, Arc<RowSchema>)> = None;
        for (row, val) in rows.iter_mut().zip(vals) {
            let ty = val.infer_type()?;
            let new_schema = match &cached {
                Some((base, cached_ty, ext))
                    if Arc::ptr_eq(base, &row.schema) && *cached_ty == ty =>
                {
                    Arc::clone(ext)
                }
                _ => {
                    let mut extended = (*row.schema).clone();
                    extended.push_col(ColMeta {
                        qualifier: None,
                        name: Arc::clone(name),
                        ty: ty.clone(),
                    });
                    let ext_arc = Arc::new(extended);
                    cached = Some((Arc::clone(&row.schema), ty, Arc::clone(&ext_arc)));
                    ext_arc
                }
            };
            row.schema = new_schema;
            row.values.push(val);
        }
        Ok(())
    }

    fn slot_to_binding(&self, slot: ColSlot) -> Option<BindingRef<'_>> {
        match slot {
            ColSlot::Col(i) => self.col(i),
            ColSlot::Pseudo(i) => {
                let meta = self.schema.pseudo_cols.get(i)?;
                let value = self.pseudo_values.get(i)?;
                Some(BindingRef {
                    qualifier: meta.qualifier.as_ref(),
                    name: &meta.name,
                    ty: &meta.ty,
                    value,
                })
            }
        }
    }

    pub(super) fn resolve_ident(&self, name: &str) -> BqResult<Option<BindingRef<'_>>> {
        Ok(self
            .schema
            .lookup_ident_slot(name)?
            .and_then(|slot| self.slot_to_binding(slot)))
    }

    pub(super) fn resolve_qualified(&self, qualifier: &str, name: &str) -> Option<BindingRef<'_>> {
        self.schema
            .lookup_qualified_slot(qualifier, name)
            .and_then(|slot| self.slot_to_binding(slot))
    }

    /// Collects the ordered visible columns belonging to a table alias / range variable.
    pub(super) fn resolve_range_var_cols(&self, name: &str) -> Vec<BindingRef<'_>> {
        let from_pseudo: Vec<BindingRef<'_>> = self
            .iter_pseudo()
            .filter(|c| {
                !c.name.starts_with("_PARTITION")
                    && c.qualifier.is_some_and(|q| q.eq_ignore_ascii_case(name))
            })
            .collect();
        if !from_pseudo.is_empty() {
            return from_pseudo;
        }
        self.iter_cols()
            .filter(|c| {
                !c.name.starts_with("__win_")
                    && c.qualifier.is_some_and(|q| q.eq_ignore_ascii_case(name))
            })
            .collect()
    }

    #[must_use]
    pub(super) fn resolve_range_var_struct(&self, name: &str) -> Option<BqValue> {
        let cols = self.resolve_range_var_cols(name);
        if cols.is_empty() {
            return None;
        }
        Some(BqValue::Struct(
            cols.into_iter()
                .map(|c| BqStructFieldValue {
                    name: Some(c.name.as_ref().to_owned()),
                    value: c.value.clone(),
                })
                .collect(),
        ))
    }

    #[must_use]
    pub(super) fn resolve_range_var_type(&self, name: &str) -> Option<BqType> {
        let cols = self.resolve_range_var_cols(name);
        if cols.is_empty() {
            return None;
        }
        Some(BqType::Struct(
            cols.into_iter()
                .map(|c| StructField::named(c.name.as_ref().to_owned(), c.ty.clone()))
                .collect(),
        ))
    }

    #[must_use]
    pub(super) fn to_struct_value(&self) -> BqValue {
        BqValue::Struct(
            self.iter_cols()
                .map(|c| BqStructFieldValue {
                    name: Some(c.name.as_ref().to_owned()),
                    value: c.value.clone(),
                })
                .collect(),
        )
    }
}

/// Zero-copy borrowed view of either a single row or a pair of joined rows.
#[derive(Debug, Clone, Copy)]
pub(super) enum RowRef<'a> {
    Single(&'a EvalRow),
    Joined {
        left: &'a EvalRow,
        right: &'a EvalRow,
    },
}

impl<'a> RowRef<'a> {
    pub(super) fn resolve_ident(self, name: &str) -> BqResult<Option<BindingRef<'a>>> {
        match self {
            Self::Single(row) => row.resolve_ident(name),
            Self::Joined { left, right } => {
                let l_slot = left.schema.lookup_ident_slot(name)?;
                let r_slot = right.schema.lookup_ident_slot(name)?;
                match (l_slot, r_slot) {
                    (Some(ColSlot::Col(_)), Some(ColSlot::Col(_))) => {
                        Err(BqError::query(format!("Column name {name} is ambiguous")))
                    }
                    (Some(ColSlot::Col(i)), _) => Ok(left.col(i)),
                    (_, Some(ColSlot::Col(i))) => Ok(right.col(i)),
                    (Some(slot), _) => Ok(left.slot_to_binding(slot)),
                    (None, Some(slot)) => Ok(right.slot_to_binding(slot)),
                    (None, None) => Ok(None),
                }
            }
        }
    }

    pub(super) fn resolve_qualified(self, qualifier: &str, name: &str) -> Option<BindingRef<'a>> {
        match self {
            Self::Single(row) => row.resolve_qualified(qualifier, name),
            Self::Joined { left, right } => left
                .resolve_qualified(qualifier, name)
                .or_else(|| right.resolve_qualified(qualifier, name)),
        }
    }

    #[must_use]
    pub(super) fn resolve_range_var_struct(self, name: &str) -> Option<BqValue> {
        match self {
            Self::Single(row) => row.resolve_range_var_struct(name),
            Self::Joined { left, right } => {
                let mut cols = left.resolve_range_var_cols(name);
                cols.extend(right.resolve_range_var_cols(name));
                if cols.is_empty() {
                    return None;
                }
                Some(BqValue::Struct(
                    cols.into_iter()
                        .map(|c| BqStructFieldValue {
                            name: Some(c.name.as_ref().to_owned()),
                            value: c.value.clone(),
                        })
                        .collect(),
                ))
            }
        }
    }

    #[must_use]
    pub(super) fn to_owned_eval_row(self) -> EvalRow {
        match self {
            Self::Single(row) => row.clone(),
            Self::Joined { left, right } => {
                let schema = RowSchema::concat(&left.schema, &right.schema);
                EvalRow::combine_with_schema(left, right, schema)
            }
        }
    }

    #[must_use]
    pub(super) fn as_eval_row(self) -> Cow<'a, EvalRow> {
        match self {
            Self::Single(row) => Cow::Borrowed(row),
            Self::Joined { .. } => Cow::Owned(self.to_owned_eval_row()),
        }
    }
}
