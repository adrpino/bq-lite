use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use boa_engine::object::builtins::{JsArray, JsDate, JsTypedArray, JsUint8Array};
use boa_engine::object::ObjectInitializer;
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::{js_string, Context, JsObject, JsString, JsValue, Source};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::ast::{DataTypeAst, RoutineParam};
use crate::error::{BqError, BqResult};
use crate::types::coercion::coerce_value;
use crate::types::{BqStructFieldValue, BqType, BqValue, StructField};

const MAX_SAFE_JS_INT: i64 = 9_007_199_254_740_991;
const MAX_SAFE_JS_INT_F64: f64 = 9_007_199_254_740_991.0;
const LOOP_ITERATION_LIMIT: u64 = 1_000_000;
const RECURSION_LIMIT: usize = 128;
const MAX_CACHED_JS_FNS: usize = 256;

struct JsRuntime {
    ctx: Context,
    initial_global_keys: HashSet<String>,
    compiled_fns: HashMap<(String, String), JsObject>,
}

thread_local! {
    static JS_RUNTIME: RefCell<JsRuntime> = RefCell::new(new_sandboxed_runtime());
}

fn new_sandboxed_runtime() -> JsRuntime {
    let mut ctx = Context::default();
    let limits = ctx.runtime_limits_mut();
    limits.set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
    limits.set_recursion_limit(RECURSION_LIMIT);
    let global = ctx.global_object();
    let initial_global_keys = global
        .own_property_keys(&mut ctx)
        .unwrap_or_default()
        .into_iter()
        .map(|k| k.to_string())
        .collect();
    JsRuntime {
        ctx,
        initial_global_keys,
        compiled_fns: HashMap::new(),
    }
}

/// Evaluates a `BigQuery` JavaScript UDF body in a sandboxed `boa_engine` runtime.
pub(super) fn eval_js_udf(
    params: &[RoutineParam],
    args: &[BqValue],
    code: &str,
    returns: Option<&DataTypeAst>,
) -> BqResult<BqValue> {
    let target_ty = returns.map(DataTypeAst::to_bq_type).transpose()?;
    JS_RUNTIME.with(|cell| {
        let mut guard = cell.try_borrow_mut().map_err(|_| {
            BqError::semantic("Re-entrant JavaScript UDF invocation is not supported")
        })?;
        eval_in_runtime(&mut guard, params, args, code, target_ty.as_ref())
    })
}

fn get_or_compile_js_fn(rt: &mut JsRuntime, param_list: &str, code: &str) -> BqResult<JsObject> {
    let cache_key = (param_list.to_owned(), code.to_owned());
    if let Some(cached) = rt.compiled_fns.get(&cache_key) {
        return Ok(cached.clone());
    }
    let wrapped = format!("(function({param_list}) {{\n{code}\n}})");
    let fn_val = rt
        .ctx
        .eval(Source::from_bytes(wrapped.as_bytes()))
        .map_err(|e| BqError::semantic(format!("JavaScript UDF compilation error: {e}")))?;
    let fn_obj = fn_val
        .as_object()
        .ok_or_else(|| BqError::semantic("JavaScript UDF did not compile to a function"))?
        .clone();
    if rt.compiled_fns.len() >= MAX_CACHED_JS_FNS {
        rt.compiled_fns.clear();
    }
    rt.compiled_fns.insert(cache_key, fn_obj.clone());
    Ok(fn_obj)
}

fn cleanup_global_props(rt: &mut JsRuntime) {
    let global = rt.ctx.global_object();
    let Ok(keys) = global.own_property_keys(&mut rt.ctx) else {
        return;
    };
    for key in keys {
        if !rt.initial_global_keys.contains(&key.to_string()) {
            if let PropertyKey::String(_) | PropertyKey::Index(_) = key {
                let _ = global.delete_property_or_throw(key, &mut rt.ctx);
            }
        }
    }
}

fn eval_in_runtime(
    rt: &mut JsRuntime,
    params: &[RoutineParam],
    args: &[BqValue],
    code: &str,
    target_ty: Option<&BqType>,
) -> BqResult<BqValue> {
    let param_list = params
        .iter()
        .map(|p| p.name.value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let fn_obj = get_or_compile_js_fn(rt, &param_list, code)?;

    let mut js_args = Vec::with_capacity(params.len());
    for (idx, _) in params.iter().enumerate() {
        let val = args.get(idx).unwrap_or(&BqValue::Null);
        js_args.push(bq_to_js(val, &mut rt.ctx)?);
    }

    let call_res = fn_obj
        .call(&JsValue::undefined(), &js_args, &mut rt.ctx)
        .map_err(|e| BqError::semantic(format!("JavaScript UDF runtime error: {e}")));
    let out = match call_res {
        Ok(val) => js_to_bq(&val, target_ty, &mut rt.ctx),
        Err(err) => Err(err),
    };
    cleanup_global_props(rt);
    out
}

fn js_err<T>(prefix: &str, res: boa_engine::JsResult<T>) -> BqResult<T> {
    res.map_err(|e| BqError::semantic(format!("{prefix}: {e}")))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Values within +/-MAX_SAFE_JS_INT are exact in IEEE-754 f64"
)]
fn bq_to_js(val: &BqValue, ctx: &mut Context) -> BqResult<JsValue> {
    match val {
        BqValue::Null => Ok(JsValue::null()),
        BqValue::Bool(b) => Ok(JsValue::new(*b)),
        BqValue::Int64(n) => {
            if (-MAX_SAFE_JS_INT..=MAX_SAFE_JS_INT).contains(n) {
                Ok(JsValue::new(*n as f64))
            } else {
                Ok(JsValue::new(JsString::from(n.to_string())))
            }
        }
        BqValue::Float64(f) => Ok(JsValue::new(*f)),
        BqValue::Numeric(n) => Ok(JsValue::new(n.to_f64())),
        BqValue::BigNumeric(n) => Ok(JsValue::new(JsString::from(n.to_string()))),
        BqValue::String(s) | BqValue::Geography(s) => Ok(JsValue::new(JsString::from(s.as_str()))),
        BqValue::Bytes(bytes) => {
            let arr = js_err(
                "Failed to convert BYTES to Uint8Array",
                JsUint8Array::from_iter(bytes.iter().copied(), ctx),
            )?;
            Ok(arr.into())
        }
        BqValue::Timestamp(ts) => {
            let date = JsDate::new(ctx);
            let ms = ts.timestamp_millis() as f64;
            let set_time = js_err(
                "Date.setTime lookup failed",
                date.get(js_string!("setTime"), ctx),
            )?;
            if let Some(f) = set_time.as_object() {
                let _ = f.call(&date.clone().into(), &[JsValue::new(ms)], ctx);
            }
            Ok(date.into())
        }
        BqValue::Date(d) => {
            let dt = d.and_hms_opt(0, 0, 0).map_or_else(
                || Utc.timestamp_millis_opt(0).single(),
                |ndt| Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc)),
            );
            dt.map_or_else(
                || Ok(JsValue::new(JsString::from(d.to_string()))),
                |ts| bq_to_js(&BqValue::Timestamp(ts), ctx),
            )
        }
        BqValue::Datetime(dt) => Ok(JsValue::new(JsString::from(dt.to_string()))),
        BqValue::Time(t) => Ok(JsValue::new(JsString::from(t.to_string()))),
        BqValue::Interval(iv) => Ok(JsValue::new(JsString::from(iv.to_string()))),
        BqValue::Range(r) => Ok(JsValue::new(JsString::from(r.to_string()))),
        BqValue::Json(j) => js_err(
            "Failed to convert JSON to JS value",
            JsValue::from_json(j, ctx),
        ),
        BqValue::Array(items) => {
            let arr = JsArray::new(ctx);
            for item in items {
                let jv = bq_to_js(item, ctx)?;
                js_err("Failed to push JS array item", arr.push(jv, ctx))?;
            }
            Ok(arr.into())
        }
        BqValue::Struct(fields) => {
            let mut init = ObjectInitializer::new(ctx);
            for (idx, field) in fields.iter().enumerate() {
                let key = field.name.clone().unwrap_or_else(|| format!("f{idx}"));
                let jv = bq_to_js(&field.value, init.context())?;
                init.property(JsString::from(key), jv, Attribute::all());
            }
            Ok(init.build().into())
        }
    }
}

fn js_to_bq(val: &JsValue, target_ty: Option<&BqType>, ctx: &mut Context) -> BqResult<BqValue> {
    if val.is_null_or_undefined() {
        return Ok(BqValue::Null);
    }
    if let Some(ty) = target_ty {
        return js_to_bq_typed(val, ty, ctx);
    }
    js_to_bq_dynamic(val, ctx)
}

fn js_to_bq_typed(val: &JsValue, ty: &BqType, ctx: &mut Context) -> BqResult<BqValue> {
    match ty {
        BqType::Json => {
            let j = js_err("Failed to convert JS value to JSON", val.to_json(ctx))?;
            Ok(BqValue::Json(Box::new(j)))
        }
        BqType::Bytes => js_to_bytes(val, ctx),
        BqType::Timestamp => js_to_timestamp(val, ctx),
        BqType::Date => js_to_date(val, ctx),
        BqType::Array(elem_ty) => js_to_array(val, Some(elem_ty.as_ref()), ctx),
        BqType::Struct(fields) => js_to_typed_struct(val, fields, ctx),
        BqType::UntypedNull
        | BqType::Bool
        | BqType::Int64
        | BqType::Float64
        | BqType::Numeric
        | BqType::BigNumeric
        | BqType::String
        | BqType::Datetime
        | BqType::Time
        | BqType::Interval
        | BqType::Geography
        | BqType::Range(_) => {
            let raw = js_to_bq_dynamic(val, ctx)?;
            coerce_value(raw, ty)
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Exact integer JS numbers in i64 range are checked before cast"
)]
fn js_to_bq_dynamic(val: &JsValue, ctx: &mut Context) -> BqResult<BqValue> {
    if val.is_null_or_undefined() {
        return Ok(BqValue::Null);
    }
    if let Some(b) = val.as_boolean() {
        return Ok(BqValue::Bool(b));
    }
    if let Some(f) = val.as_number() {
        if f.is_finite() && f.fract() == 0.0 && f.abs() <= MAX_SAFE_JS_INT_F64 {
            return Ok(BqValue::Int64(f as i64));
        }
        return Ok(BqValue::Float64(f));
    }
    if let Some(s) = val.as_string() {
        return Ok(BqValue::String(s.to_std_string_escaped()));
    }
    if let Some(bi) = val.as_bigint() {
        let s = bi.to_string();
        return s
            .parse::<i64>()
            .map(BqValue::Int64)
            .map_err(|_| BqError::semantic(format!("JavaScript BigInt '{s}' out of INT64 range")));
    }
    if let Some(obj) = val.as_object() {
        return js_object_to_bq(obj, ctx);
    }
    Err(BqError::semantic("Unsupported JavaScript return value"))
}

fn js_object_to_bq(obj: &JsObject, ctx: &mut Context) -> BqResult<BqValue> {
    if obj.is_array() {
        return js_to_array(&JsValue::from(obj.clone()), None, ctx);
    }
    if let Ok(typed) = JsTypedArray::from_object(obj.clone()) {
        return extract_typed_array_bytes(&typed, ctx);
    }
    if let Some(ts) = try_extract_js_date(obj, ctx)? {
        return Ok(BqValue::Timestamp(ts));
    }
    let keys = js_err(
        "Failed to inspect JS object keys",
        obj.own_property_keys(ctx),
    )?;
    let mut fields = Vec::with_capacity(keys.len());
    for key in keys {
        let name = key.to_string();
        let prop = js_err("Failed to read JS object property", obj.get(key, ctx))?;
        let bv = js_to_bq(&prop, None, ctx)?;
        fields.push(BqStructFieldValue {
            name: Some(name),
            value: bv,
        });
    }
    Ok(BqValue::Struct(fields))
}

fn js_to_array(val: &JsValue, elem_ty: Option<&BqType>, ctx: &mut Context) -> BqResult<BqValue> {
    let obj = val
        .as_object()
        .ok_or_else(|| BqError::semantic("Expected JavaScript Array for BigQuery ARRAY"))?;
    let arr = js_err(
        "Expected JavaScript Array for BigQuery ARRAY",
        JsArray::from_object(obj.clone()),
    )?;
    let len = js_err("Failed to read JS array length", arr.length(ctx))?;
    let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    for i in 0..len {
        let item = js_err(
            "Failed to read JS array element",
            arr.at(i64::try_from(i).unwrap_or(0), ctx),
        )?;
        out.push(js_to_bq(&item, elem_ty, ctx)?);
    }
    Ok(BqValue::Array(out))
}

fn js_to_typed_struct(
    val: &JsValue,
    fields: &[StructField],
    ctx: &mut Context,
) -> BqResult<BqValue> {
    let obj = val
        .as_object()
        .ok_or_else(|| BqError::semantic("Expected JavaScript Object for BigQuery STRUCT"))?;
    let mut out = Vec::with_capacity(fields.len());
    for (idx, field) in fields.iter().enumerate() {
        let key_name = field.name.clone().unwrap_or_else(|| format!("f{idx}"));
        let prop = js_err(
            "Failed to read JS STRUCT field",
            obj.get(JsString::from(key_name), ctx),
        )?;
        let bv = js_to_bq(&prop, Some(&field.ty), ctx)?;
        out.push(BqStructFieldValue {
            name: field.name.clone(),
            value: bv,
        });
    }
    Ok(BqValue::Struct(out))
}

fn js_to_bytes(val: &JsValue, ctx: &mut Context) -> BqResult<BqValue> {
    if let Some(s) = val.as_string() {
        let text = s.to_std_string_escaped();
        let decoded = BASE64
            .decode(text.as_bytes())
            .unwrap_or_else(|_| text.into_bytes());
        return Ok(BqValue::Bytes(decoded));
    }
    if let Some(obj) = val.as_object() {
        if let Ok(typed) = JsTypedArray::from_object(obj.clone()) {
            return extract_typed_array_bytes(&typed, ctx);
        }
    }
    Err(BqError::semantic(
        "Expected Uint8Array or base64 STRING for BigQuery BYTES",
    ))
}

fn extract_typed_array_bytes(typed: &JsTypedArray, ctx: &mut Context) -> BqResult<BqValue> {
    let len = js_err("Failed to read Uint8Array length", typed.length(ctx))?;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let elem = js_err(
            "Failed to read Uint8Array element",
            typed.at(i64::try_from(i).unwrap_or(0), ctx),
        )?;
        let byte = js_err("Failed to convert Uint8Array element", elem.to_u32(ctx))?;
        out.push(u8::try_from(byte & 0xFF).unwrap_or(0));
    }
    Ok(BqValue::Bytes(out))
}

fn try_extract_js_date(obj: &JsObject, ctx: &mut Context) -> BqResult<Option<DateTime<Utc>>> {
    if !obj.is::<boa_engine::builtins::date::Date>() {
        return Ok(None);
    }
    let get_time = js_err(
        "Date.getTime lookup failed",
        obj.get(js_string!("getTime"), ctx),
    )?;
    let Some(f) = get_time.as_object() else {
        return Ok(None);
    };
    let ms_val = js_err(
        "Date.getTime call failed",
        f.call(&JsValue::from(obj.clone()), &[], ctx),
    )?;
    let ms = ms_val
        .as_number()
        .ok_or_else(|| BqError::semantic("Invalid JS Date timestamp"))?;
    if !ms.is_finite() {
        return Err(BqError::semantic("Invalid JS Date (NaN)"));
    }
    #[expect(clippy::cast_possible_truncation, reason = "Epoch millis fit in i64")]
    let ms_i64 = ms.trunc() as i64;
    let dt = Utc
        .timestamp_millis_opt(ms_i64)
        .single()
        .ok_or_else(|| BqError::semantic("JS Date out of BigQuery TIMESTAMP range"))?;
    Ok(Some(dt))
}

fn js_to_timestamp(val: &JsValue, ctx: &mut Context) -> BqResult<BqValue> {
    if let Some(obj) = val.as_object() {
        if let Some(ts) = try_extract_js_date(obj, ctx)? {
            return Ok(BqValue::Timestamp(ts));
        }
    }
    let raw = js_to_bq_dynamic(val, ctx)?;
    coerce_value(raw, &BqType::Timestamp)
}

fn js_to_date(val: &JsValue, ctx: &mut Context) -> BqResult<BqValue> {
    if let Some(obj) = val.as_object() {
        if let Some(ts) = try_extract_js_date(obj, ctx)? {
            return Ok(BqValue::Date(ts.date_naive()));
        }
    }
    if let Some(s) = val.as_string() {
        let text = s.to_std_string_escaped();
        if let Ok(d) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
            return Ok(BqValue::Date(d));
        }
    }
    let raw = js_to_bq_dynamic(val, ctx)?;
    coerce_value(raw, &BqType::Date)
}
