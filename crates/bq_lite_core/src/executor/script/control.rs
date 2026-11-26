use crate::ast::{
    AssertStmt, BeginEndBlock, CallStmt, CaseStmt, ExecuteImmediateStmt, ForInStmt, IfStmt,
    ParamMode, RaiseStmt, RepeatStmt, Statement, WhileStmt,
};
use crate::catalog::{Catalog, RoutineMeta};
use crate::error::{BqError, BqResult};
use crate::executor::context::EvalRow;
use crate::executor::evaluator::{eval_expr, values_equal};
use crate::executor::query::execute_query;
use crate::parser::parse_script;
use crate::storage::StorageEngine;
use crate::types::BqValue;

use super::{exec_block, exec_single_stmt, FlowSignal, ScriptState};

const MAX_LOOP_ITERS: usize = 100_000;

pub(super) fn exec_if(
    stmt: &IfStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    for branch in &stmt.branches {
        let cond = eval_in_state(&branch.condition, catalog, storage, state)?;
        if matches!(cond, BqValue::Bool(true)) {
            return exec_block(&branch.body, catalog, storage, state);
        }
    }
    if let Some(ref else_body) = stmt.else_body {
        return exec_block(else_body, catalog, storage, state);
    }
    Ok(FlowSignal::Normal)
}

pub(super) fn exec_case_stmt(
    stmt: &CaseStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    let op_val = stmt
        .operand
        .as_ref()
        .map(|e| eval_in_state(e, catalog, storage, state))
        .transpose()?;
    for branch in &stmt.branches {
        let cond = eval_in_state(&branch.condition, catalog, storage, state)?;
        let matched = match &op_val {
            Some(ov) => !ov.is_null() && !cond.is_null() && values_equal(ov, &cond)?,
            None => matches!(cond, BqValue::Bool(true)),
        };
        if matched {
            return exec_block(&branch.body, catalog, storage, state);
        }
    }
    if let Some(ref else_body) = stmt.else_body {
        return exec_block(else_body, catalog, storage, state);
    }
    Ok(FlowSignal::Normal)
}

pub(super) fn exec_loop(
    body: &[Statement],
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    for _ in 0..MAX_LOOP_ITERS {
        match exec_block(body, catalog, storage, state)? {
            FlowSignal::Normal | FlowSignal::Continue => {}
            FlowSignal::Break => return Ok(FlowSignal::Normal),
            FlowSignal::Return => return Ok(FlowSignal::Return),
        }
    }
    Err(BqError::semantic("LOOP exceeded maximum iteration limit"))
}

pub(super) fn exec_while(
    stmt: &WhileStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    for _ in 0..MAX_LOOP_ITERS {
        let cond = eval_in_state(&stmt.condition, catalog, storage, state)?;
        if !matches!(cond, BqValue::Bool(true)) {
            return Ok(FlowSignal::Normal);
        }
        match exec_block(&stmt.body, catalog, storage, state)? {
            FlowSignal::Normal | FlowSignal::Continue => {}
            FlowSignal::Break => return Ok(FlowSignal::Normal),
            FlowSignal::Return => return Ok(FlowSignal::Return),
        }
    }
    Err(BqError::semantic(
        "WHILE loop exceeded maximum iteration limit",
    ))
}

pub(super) fn exec_repeat(
    stmt: &RepeatStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    for _ in 0..MAX_LOOP_ITERS {
        match exec_block(&stmt.body, catalog, storage, state)? {
            FlowSignal::Normal | FlowSignal::Continue => {}
            FlowSignal::Break => return Ok(FlowSignal::Normal),
            FlowSignal::Return => return Ok(FlowSignal::Return),
        }
        let until = eval_in_state(&stmt.until, catalog, storage, state)?;
        if matches!(until, BqValue::Bool(true)) {
            return Ok(FlowSignal::Normal);
        }
    }
    Err(BqError::semantic(
        "REPEAT loop exceeded maximum iteration limit",
    ))
}

pub(super) fn exec_for_in(
    stmt: &ForInStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    let qr = {
        let ctx = state.make_ctx(catalog, storage);
        execute_query(&stmt.query, &ctx)?
    };
    let var_key = stmt.var.value.to_ascii_lowercase();
    let prev = state.vars.get(&var_key).cloned();

    for row in qr.rows {
        let fields: Vec<crate::types::BqStructFieldValue> = qr
            .columns
            .iter()
            .zip(row)
            .map(|(c, v)| crate::types::BqStructFieldValue {
                name: Some(c.name.clone()),
                value: v,
            })
            .collect();
        state.vars.insert(var_key.clone(), BqValue::Struct(fields));
        match exec_block(&stmt.body, catalog, storage, state)? {
            FlowSignal::Normal | FlowSignal::Continue => {}
            FlowSignal::Break => break,
            FlowSignal::Return => {
                restore_var(state, &var_key, prev);
                return Ok(FlowSignal::Return);
            }
        }
    }
    restore_var(state, &var_key, prev);
    Ok(FlowSignal::Normal)
}

fn restore_var(state: &mut ScriptState, key: &str, prev: Option<BqValue>) {
    match prev {
        Some(v) => {
            state.vars.insert(key.to_owned(), v);
        }
        None => {
            state.vars.remove(key);
        }
    }
}

pub(super) fn exec_begin_end(
    block: &BeginEndBlock,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    match exec_block(&block.body, catalog, storage, state) {
        Ok(sig) => Ok(sig),
        Err(err) => {
            let Some(ref handler) = block.exception_body else {
                return Err(err);
            };
            state
                .sys_vars
                .insert("error.message".to_owned(), BqValue::String(err.message));
            state.sys_vars.insert(
                "error.statement_text".to_owned(),
                BqValue::String(String::new()),
            );
            exec_block(handler, catalog, storage, state)
        }
    }
}

pub(super) fn exec_execute_immediate(
    stmt: &ExecuteImmediateStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    let sql_val = eval_in_state(&stmt.sql, catalog, storage, state)?;
    let BqValue::String(sql_str) = sql_val else {
        return Err(BqError::query("EXECUTE IMMEDIATE requires STRING SQL"));
    };

    let mut named = std::collections::BTreeMap::new();
    let mut positional = Vec::new();
    for (expr, alias) in &stmt.using {
        let v = eval_in_state(expr, catalog, storage, state)?;
        if let Some(a) = alias {
            named.insert(a.value.clone(), v);
        } else {
            positional.push(v);
        }
    }

    let stmts = parse_script(&sql_str)?;
    let prev_params = std::mem::replace(&mut state.params, named);
    let prev_pos = std::mem::replace(&mut state.positional_params, positional);
    let mut res = Ok(FlowSignal::Normal);
    for s in &stmts {
        res = exec_single_stmt(s, catalog, storage, state);
        if res.is_err() {
            break;
        }
    }
    state.params = prev_params;
    state.positional_params = prev_pos;
    let sig = res?;

    if !stmt.into.is_empty() {
        let row = state.last_result.rows.first().cloned().unwrap_or_default();
        if row.len() != stmt.into.len() {
            return Err(BqError::query(
                "EXECUTE IMMEDIATE INTO variable count mismatch",
            ));
        }
        for (var_ident, val) in stmt.into.iter().zip(row) {
            state.vars.insert(var_ident.value.to_ascii_lowercase(), val);
        }
    }
    Ok(sig)
}

pub(super) fn exec_call(
    stmt: &CallStmt,
    catalog: &mut Catalog,
    storage: &mut StorageEngine,
    state: &mut ScriptState,
) -> BqResult<FlowSignal> {
    let tref = catalog.resolve_table_name(&stmt.name)?;
    let proc_def = {
        let ds = catalog.get_dataset(&tref.project, &tref.dataset)?;
        let routine = ds.routines.get(&tref.table).ok_or_else(|| {
            BqError::not_found(format!(
                "Procedure {}:{}.{} not found",
                tref.project, tref.dataset, tref.table
            ))
        })?;
        let RoutineMeta::Procedure(cp) = routine.as_ref() else {
            return Err(BqError::query("CALL target is not a procedure"));
        };
        cp.clone()
    };

    if proc_def.params.len() != stmt.args.len() {
        return Err(BqError::query(format!(
            "Procedure expects {} arguments, got {}",
            proc_def.params.len(),
            stmt.args.len()
        )));
    }

    let mut proc_state = ScriptState::default();
    let mut arg_vals = Vec::with_capacity(stmt.args.len());
    for arg in &stmt.args {
        arg_vals.push(eval_in_state(arg, catalog, storage, state)?);
    }
    for (param, val) in proc_def.params.iter().zip(arg_vals) {
        let init = if param.mode == Some(ParamMode::Out) {
            BqValue::Null
        } else {
            val
        };
        proc_state
            .vars
            .insert(param.name.value.to_ascii_lowercase(), init);
    }

    let _ = exec_block(&proc_def.body, catalog, storage, &mut proc_state)?;
    for (param, arg_expr) in proc_def.params.iter().zip(&stmt.args) {
        if matches!(param.mode, Some(ParamMode::Out | ParamMode::InOut)) {
            let out_val = proc_state
                .vars
                .get(&param.name.value.to_ascii_lowercase())
                .cloned()
                .unwrap_or(BqValue::Null);
            if let crate::ast::Expr::Ident(id) = arg_expr {
                state.vars.insert(id.value.to_ascii_lowercase(), out_val);
            }
        }
    }
    state.last_result = proc_state.last_result;
    Ok(FlowSignal::Normal)
}

pub(super) fn exec_raise(
    stmt: &RaiseStmt,
    catalog: &Catalog,
    storage: &StorageEngine,
    state: &ScriptState,
) -> BqResult<FlowSignal> {
    let msg = match &stmt.message {
        Some(expr) => eval_in_state(expr, catalog, storage, state)?.to_string(),
        None => "User-raised error".to_owned(),
    };
    Err(BqError::semantic(msg))
}

pub(super) fn exec_assert(
    stmt: &AssertStmt,
    catalog: &Catalog,
    storage: &StorageEngine,
    state: &ScriptState,
) -> BqResult<FlowSignal> {
    let cond = eval_in_state(&stmt.condition, catalog, storage, state)?;
    if matches!(cond, BqValue::Bool(true)) {
        return Ok(FlowSignal::Normal);
    }
    let desc = match &stmt.description {
        Some(e) => eval_in_state(e, catalog, storage, state)?.to_string(),
        None => "Assertion failed".to_owned(),
    };
    Err(BqError::semantic(desc))
}

pub(super) fn eval_in_state(
    expr: &crate::ast::Expr,
    catalog: &Catalog,
    storage: &StorageEngine,
    state: &ScriptState,
) -> BqResult<BqValue> {
    let ctx = state.make_ctx(catalog, storage);
    eval_expr(expr, &EvalRow::default(), &ctx)
}
