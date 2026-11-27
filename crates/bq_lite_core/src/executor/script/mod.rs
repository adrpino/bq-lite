mod control;

use std::collections::BTreeMap;

use chrono::Utc;

use crate::ast::{DeclareStmt, SetStmt, Statement, TransactionStmt};
use crate::catalog::Catalog;
use crate::error::{BqError, BqResult};
use crate::executor::context::{ExecContext, ScriptBindings};
use crate::executor::ddl::execute_ddl_with_bindings;
use crate::executor::dml::execute_dml_with_bindings;
use crate::executor::query::execute_query;
use crate::executor::result::QueryResult;
use crate::storage::StorageEngine;
use crate::types::coercion::coerce_value;
use crate::types::BqValue;

use control::{
    eval_in_state, exec_assert, exec_begin_end, exec_call, exec_case_stmt, exec_execute_immediate,
    exec_for_in, exec_if, exec_loop, exec_raise, exec_repeat, exec_while,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlowSignal {
    Normal,
    Break,
    Continue,
    Return,
}

#[derive(Debug, Default)]
pub(in crate::executor) struct ScriptState {
    pub vars: BTreeMap<String, BqValue>,
    pub sys_vars: BTreeMap<String, BqValue>,
    pub params: BTreeMap<String, BqValue>,
    pub positional_params: Vec<BqValue>,
    pub txn_snapshot: Option<(Catalog, StorageEngine)>,
    pub last_result: QueryResult,
    pub governance: super::context::GovernanceConfig,
}

impl ScriptState {
    pub(in crate::executor) fn make_ctx<'a>(
        &'a self,
        catalog: &'a Catalog,
        storage: &'a StorageEngine,
    ) -> ExecContext<'a> {
        ExecContext {
            catalog,
            storage,
            ctes: BTreeMap::new(),
            params: self.params.clone(),
            positional_params: self.positional_params.clone(),
            vars: self.vars.clone(),
            sys_vars: self.sys_vars.clone(),
            outer_rows: Vec::new(),
            now: Utc::now(),
            governance: self.governance.clone(),
        }
    }
}

pub(in crate::executor) fn execute_script_stmts(
    stmts: &[Statement],
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
) -> BqResult<QueryResult> {
    execute_script_stmts_with_params(stmts, catalog, storage, &BTreeMap::new(), &[])
}

pub(in crate::executor) fn execute_script_stmts_with_params(
    stmts: &[Statement],
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    params: &BTreeMap<String, BqValue>,
    positional: &[BqValue],
) -> BqResult<QueryResult> {
    execute_script_stmts_with_governance(
        stmts,
        catalog,
        storage,
        params,
        positional,
        &super::context::GovernanceConfig::default(),
    )
}

pub(in crate::executor) fn execute_script_stmts_with_governance(
    stmts: &[Statement],
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    params: &BTreeMap<String, BqValue>,
    positional: &[BqValue],
    governance: &super::context::GovernanceConfig,
) -> BqResult<QueryResult> {
    let mut state = ScriptState {
        params: params.clone(),
        positional_params: positional.to_vec(),
        governance: governance.clone(),
        ..ScriptState::default()
    };
    state
        .sys_vars
        .insert("row_count".to_owned(), BqValue::Int64(0));
    let _ = exec_block(stmts, catalog, storage, &mut state)?;
    Ok(state.last_result)
}

pub(super) fn exec_block(
    stmts: &[Statement],
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    for stmt in stmts {
        let sig = exec_single_stmt(stmt, catalog, storage, state)?;
        if sig != FlowSignal::Normal {
            return Ok(sig);
        }
    }
    Ok(FlowSignal::Normal)
}

pub(super) fn exec_single_stmt(
    stmt: &Statement,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    if let Statement::Query(q) = stmt {
        let ctx = state.make_ctx(catalog, storage);
        let qr = execute_query(q, &ctx)?;
        let cnt = i64::try_from(qr.rows.len()).unwrap_or(i64::MAX);
        state
            .sys_vars
            .insert("row_count".to_owned(), BqValue::Int64(cnt));
        state.last_result = qr;
        return Ok(FlowSignal::Normal);
    }
    let bindings = ScriptBindings {
        vars: Some(&state.vars),
        params: Some(&state.params),
        positional: &state.positional_params,
    };
    if let Some(res) = execute_ddl_with_bindings(stmt, catalog, storage, bindings)? {
        state.last_result = res;
        return Ok(FlowSignal::Normal);
    }
    if let Some(res) = execute_dml_with_bindings(stmt, catalog, storage, bindings)? {
        let cnt = i64::try_from(res.rows_affected).unwrap_or(i64::MAX);
        state
            .sys_vars
            .insert("row_count".to_owned(), BqValue::Int64(cnt));
        state.last_result = res;
        return Ok(FlowSignal::Normal);
    }

    match stmt {
        Statement::Declare(d) => exec_declare(d, catalog, storage, state),
        Statement::Set(s) => exec_set(s, catalog, storage, state),
        Statement::If(i) => exec_if(i, catalog, storage, state),
        Statement::Case(c) => exec_case_stmt(c, catalog, storage, state),
        Statement::Loop(body) => exec_loop(body, catalog, storage, state),
        Statement::While(w) => exec_while(w, catalog, storage, state),
        Statement::Repeat(r) => exec_repeat(r, catalog, storage, state),
        Statement::ForIn(f) => exec_for_in(f, catalog, storage, state),
        Statement::BeginEnd(b) => exec_begin_end(b, catalog, storage, state),
        Statement::ExecuteImmediate(ei) => exec_execute_immediate(ei, catalog, storage, state),
        Statement::Call(c) => exec_call(c, catalog, storage, state),
        Statement::Raise(r) => exec_raise(r, catalog, storage, state),
        Statement::Assert(a) => exec_assert(a, catalog, storage, state),
        Statement::Transaction(tx) => Ok(exec_transaction(*tx, catalog, storage, state)),
        Statement::Break => Ok(FlowSignal::Break),
        Statement::Continue => Ok(FlowSignal::Continue),
        Statement::Return => Ok(FlowSignal::Return),
        Statement::Query(_)
        | Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_)
        | Statement::Merge(_)
        | Statement::Truncate(_)
        | Statement::CreateTable(_)
        | Statement::AlterTable(_)
        | Statement::AlterSchema(_)
        | Statement::AlterView(_)
        | Statement::Drop(_)
        | Statement::CreateSchema(_)
        | Statement::CreateView(_)
        | Statement::CreateFunction(_)
        | Statement::CreateProcedure(_)
        | Statement::CreateRowAccessPolicy(_)
        | Statement::ExportData(_)
        | Statement::LoadData(_) => Ok(FlowSignal::Normal),
    }
}

fn exec_declare(
    stmt: &DeclareStmt,
    catalog: &Catalog,
    storage: &StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    let default_val = match &stmt.default_expr {
        Some(e) => {
            let v = eval_in_state(e, catalog, storage, state)?;
            match &stmt.ty {
                Some(ty) => coerce_value(v, &ty.to_bq_type()?)?,
                None => v,
            }
        }
        None => BqValue::Null,
    };
    for var in &stmt.vars {
        let key = var.value.to_ascii_lowercase();
        if state.vars.contains_key(&key) {
            return Err(BqError::query(format!(
                "Variable '{}' is already declared",
                var.value
            )));
        }
        state.vars.insert(key, default_val.clone());
    }
    Ok(FlowSignal::Normal)
}

fn exec_set(
    stmt: &SetStmt,
    catalog: &Catalog,
    storage: &StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    if stmt.targets.len() == 1 {
        let val = eval_in_state(&stmt.value, catalog, storage, state)?;
        if let Some(first) = stmt.targets.first() {
            state.vars.insert(first.value.to_ascii_lowercase(), val);
        }
        return Ok(FlowSignal::Normal);
    }
    if let crate::ast::Expr::Subquery(ref q) = stmt.value {
        let ctx = state.make_ctx(catalog, storage);
        let qr = execute_query(q, &ctx)?;
        let row = qr.rows.into_iter().next().unwrap_or_default();
        if row.len() != stmt.targets.len() {
            return Err(BqError::query("Multi-variable SET column count mismatch"));
        }
        for (target, val) in stmt.targets.iter().zip(row) {
            state.vars.insert(target.value.to_ascii_lowercase(), val);
        }
        return Ok(FlowSignal::Normal);
    }
    let val = eval_in_state(&stmt.value, catalog, storage, state)?;
    let BqValue::Struct(fields) = val else {
        return Err(BqError::query(
            "Multi-variable SET requires a STRUCT or subquery value",
        ));
    };
    if fields.len() != stmt.targets.len() {
        return Err(BqError::query("Multi-variable SET field count mismatch"));
    }
    for (target, field) in stmt.targets.iter().zip(fields) {
        state
            .vars
            .insert(target.value.to_ascii_lowercase(), field.value);
    }
    Ok(FlowSignal::Normal)
}

fn exec_transaction(
    tx: TransactionStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> FlowSignal {
    match tx {
        TransactionStmt::Begin => {
            state.txn_snapshot = Some((catalog.clone(), storage.clone()));
        }
        TransactionStmt::Commit => {
            state.txn_snapshot = None;
        }
        TransactionStmt::Rollback => {
            if let Some((cat, st)) = state.txn_snapshot.take() {
                *catalog = cat;
                *storage = st;
            }
        }
    }
    FlowSignal::Normal
}
