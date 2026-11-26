use std::collections::BTreeMap;

use bq_lite_core::catalog::ColumnSchema;
use bq_lite_core::types::coercion::cast_value;
use bq_lite_core::{BqError, BqResult, BqStructFieldValue, BqType, BqValue, StructField};
use chrono::{DateTime, Duration, NaiveDate};
use prost::encoding::{decode_key, decode_varint, WireType};

use super::proto::{DescriptorProto, FieldDescriptorProto};

const LABEL_REPEATED: i32 = 3;
const TYPE_DOUBLE: i32 = 1;
const TYPE_FLOAT: i32 = 2;
const TYPE_INT64: i32 = 3;
const TYPE_UINT64: i32 = 4;
const TYPE_INT32: i32 = 5;
const TYPE_FIXED64: i32 = 6;
const TYPE_FIXED32: i32 = 7;
const TYPE_BOOL: i32 = 8;
const TYPE_STRING: i32 = 9;
const TYPE_MESSAGE: i32 = 11;
const TYPE_BYTES: i32 = 12;
const TYPE_UINT32: i32 = 13;
const TYPE_ENUM: i32 = 14;
const TYPE_SFIXED32: i32 = 15;
const TYPE_SFIXED64: i32 = 16;
const TYPE_SINT32: i32 = 17;
const TYPE_SINT64: i32 = 18;

/// Decodes serialized protobuf row messages into table rows matching `table_cols`.
pub fn decode_proto_rows(
    desc: &DescriptorProto,
    serialized_rows: &[Vec<u8>],
    table_cols: &[ColumnSchema],
) -> BqResult<Vec<Vec<BqValue>>> {
    let mut out = Vec::with_capacity(serialized_rows.len());
    for raw in serialized_rows {
        let field_map = decode_message_fields(desc, raw)?;
        let mut row = Vec::with_capacity(table_cols.len());
        for col in table_cols {
            let val = match field_map
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&col.name))
                .map(|(_, v)| v.clone())
            {
                Some(v) => coerce_proto_value(v, &col.ty)?,
                None => {
                    if matches!(col.ty, BqType::Array(_)) {
                        BqValue::Array(Vec::new())
                    } else {
                        BqValue::Null
                    }
                }
            };
            if !col.nullable && val.is_null() {
                return Err(BqError::invalid_argument(format!(
                    "Required field '{}' cannot be NULL in proto_rows",
                    col.name
                )));
            }
            row.push(val);
        }
        out.push(row);
    }
    Ok(out)
}

fn decode_message_fields(
    desc: &DescriptorProto,
    mut bytes: &[u8],
) -> BqResult<BTreeMap<String, BqValue>> {
    let mut collected: BTreeMap<i32, Vec<BqValue>> = BTreeMap::new();
    while !bytes.is_empty() {
        let (tag, wire_type) = decode_key(&mut bytes)
            .map_err(|e| BqError::invalid_argument(format!("Invalid proto wire key: {e}")))?;
        let tag_i32 = i32::try_from(tag).unwrap_or(0);
        let fdesc = desc.field.iter().find(|f| f.number == Some(tag_i32));
        let Some(fd) = fdesc else {
            skip_wire_field(wire_type, &mut bytes)?;
            continue;
        };
        if fd.label == Some(LABEL_REPEATED)
            && wire_type == WireType::LengthDelimited
            && is_packable_scalar(fd.r#type.unwrap_or(TYPE_INT64))
        {
            let slice = read_len_delim(&mut bytes)?;
            let packed = decode_packed_scalars(fd.r#type.unwrap_or(TYPE_INT64), slice)?;
            collected.entry(tag_i32).or_default().extend(packed);
            continue;
        }
        let val = decode_single_field(desc, fd, wire_type, &mut bytes)?;
        collected.entry(tag_i32).or_default().push(val);
    }

    let mut out = BTreeMap::new();
    for fd in &desc.field {
        let Some(num) = fd.number else {
            continue;
        };
        let fname = fd.name.clone().unwrap_or_else(|| format!("f{num}"));
        if let Some(vals) = collected.remove(&num) {
            if fd.label == Some(LABEL_REPEATED) {
                out.insert(fname, BqValue::Array(vals));
            } else if let Some(last) = vals.into_iter().last() {
                out.insert(fname, last);
            }
        }
    }
    Ok(out)
}

const fn is_packable_scalar(ftype: i32) -> bool {
    !matches!(ftype, TYPE_STRING | TYPE_BYTES | TYPE_MESSAGE)
}

fn decode_packed_scalars(ftype: i32, mut slice: &[u8]) -> BqResult<Vec<BqValue>> {
    let mut items = Vec::new();
    while !slice.is_empty() {
        let wt = match ftype {
            TYPE_DOUBLE | TYPE_FIXED64 | TYPE_SFIXED64 => WireType::SixtyFourBit,
            TYPE_FLOAT | TYPE_FIXED32 | TYPE_SFIXED32 => WireType::ThirtyTwoBit,
            _ => WireType::Varint,
        };
        items.push(decode_scalar_field(ftype, wt, &mut slice)?);
    }
    Ok(items)
}

fn decode_single_field(
    parent_desc: &DescriptorProto,
    fd: &FieldDescriptorProto,
    wt: WireType,
    bytes: &mut &[u8],
) -> BqResult<BqValue> {
    let ftype = fd.r#type.unwrap_or(TYPE_STRING);
    if ftype == TYPE_MESSAGE {
        let sub_bytes = read_len_delim(bytes)?;
        let type_short = fd
            .type_name
            .as_deref()
            .and_then(|s| s.rsplit('.').next())
            .unwrap_or("");
        let nested = parent_desc
            .nested_type
            .iter()
            .find(|n| n.name.as_deref() == Some(type_short))
            .ok_or_else(|| {
                BqError::invalid_argument(format!("Missing nested proto descriptor '{type_short}'"))
            })?;
        let map = decode_message_fields(nested, sub_bytes)?;
        let fields = map
            .into_iter()
            .map(|(name, value)| BqStructFieldValue {
                name: Some(name),
                value,
            })
            .collect();
        return Ok(BqValue::Struct(fields));
    }
    decode_scalar_field(ftype, wt, bytes)
}

fn decode_scalar_field(ftype: i32, wt: WireType, bytes: &mut &[u8]) -> BqResult<BqValue> {
    match (ftype, wt) {
        (TYPE_DOUBLE, WireType::SixtyFourBit) => {
            Ok(BqValue::Float64(f64::from_le_bytes(read_8(bytes)?)))
        }
        (TYPE_FLOAT, WireType::ThirtyTwoBit) => Ok(BqValue::Float64(f64::from(
            f32::from_le_bytes(read_4(bytes)?),
        ))),
        (TYPE_FIXED64, WireType::SixtyFourBit) => {
            let u = u64::from_le_bytes(read_8(bytes)?);
            Ok(BqValue::Int64(i64::try_from(u).unwrap_or(i64::MAX)))
        }
        (TYPE_SFIXED64, WireType::SixtyFourBit) => {
            Ok(BqValue::Int64(i64::from_le_bytes(read_8(bytes)?)))
        }
        (TYPE_FIXED32, WireType::ThirtyTwoBit) => Ok(BqValue::Int64(i64::from(
            u32::from_le_bytes(read_4(bytes)?),
        ))),
        (TYPE_SFIXED32, WireType::ThirtyTwoBit) => Ok(BqValue::Int64(i64::from(
            i32::from_le_bytes(read_4(bytes)?),
        ))),
        (TYPE_BOOL, WireType::Varint) => Ok(BqValue::Bool(read_varint(bytes)? != 0)),
        (TYPE_SINT32, WireType::Varint) => {
            let v = u32::try_from(read_varint(bytes)?).unwrap_or(0);
            let shifted = i32::from_le_bytes((v >> 1).to_le_bytes());
            let mask = -i32::from_le_bytes((v & 1).to_le_bytes());
            Ok(BqValue::Int64(i64::from(shifted ^ mask)))
        }
        (TYPE_SINT64, WireType::Varint) => {
            let v = read_varint(bytes)?;
            let shifted = i64::from_le_bytes((v >> 1).to_le_bytes());
            let mask = -i64::from_le_bytes((v & 1).to_le_bytes());
            Ok(BqValue::Int64(shifted ^ mask))
        }
        (TYPE_INT64 | TYPE_INT32 | TYPE_ENUM, WireType::Varint) => Ok(BqValue::Int64(
            i64::from_le_bytes(read_varint(bytes)?.to_le_bytes()),
        )),
        (TYPE_UINT64 | TYPE_UINT32, WireType::Varint) => {
            let u = read_varint(bytes)?;
            Ok(BqValue::Int64(i64::try_from(u).unwrap_or(i64::MAX)))
        }
        (TYPE_STRING, WireType::LengthDelimited) => {
            let s = std::str::from_utf8(read_len_delim(bytes)?).map_err(|e| {
                BqError::invalid_argument(format!("Invalid UTF-8 in proto string: {e}"))
            })?;
            Ok(BqValue::String(s.to_owned()))
        }
        (TYPE_BYTES, WireType::LengthDelimited) => {
            Ok(BqValue::Bytes(read_len_delim(bytes)?.to_vec()))
        }
        _ => Err(BqError::invalid_argument(format!(
            "Unsupported proto field type {ftype} / wire type {wt:?}"
        ))),
    }
}

fn read_varint(bytes: &mut &[u8]) -> BqResult<u64> {
    decode_varint(bytes)
        .map_err(|e| BqError::invalid_argument(format!("Invalid proto varint: {e}")))
}

fn read_len_delim<'a>(bytes: &mut &'a [u8]) -> BqResult<&'a [u8]> {
    let len_u64 = read_varint(bytes)?;
    let len = usize::try_from(len_u64)
        .map_err(|_| BqError::invalid_argument("Proto length exceeds usize"))?;
    let head = bytes
        .get(..len)
        .ok_or_else(|| BqError::invalid_argument("Truncated proto length-delimited field"))?;
    *bytes = bytes.get(len..).unwrap_or(&[]);
    Ok(head)
}

fn read_4(bytes: &mut &[u8]) -> BqResult<[u8; 4]> {
    let head = bytes
        .get(..4)
        .ok_or_else(|| BqError::invalid_argument("Truncated 32-bit proto field"))?;
    *bytes = bytes.get(4..).unwrap_or(&[]);
    head.try_into()
        .map_err(|_| BqError::internal("4-byte proto slice"))
}

fn read_8(bytes: &mut &[u8]) -> BqResult<[u8; 8]> {
    let head = bytes
        .get(..8)
        .ok_or_else(|| BqError::invalid_argument("Truncated 64-bit proto field"))?;
    *bytes = bytes.get(8..).unwrap_or(&[]);
    head.try_into()
        .map_err(|_| BqError::internal("8-byte proto slice"))
}

fn skip_wire_field(wt: WireType, bytes: &mut &[u8]) -> BqResult<()> {
    match wt {
        WireType::Varint => {
            let _ = read_varint(bytes)?;
        }
        WireType::SixtyFourBit => {
            let _ = read_8(bytes)?;
        }
        WireType::LengthDelimited => {
            let _ = read_len_delim(bytes)?;
        }
        WireType::ThirtyTwoBit => {
            let _ = read_4(bytes)?;
        }
        WireType::StartGroup | WireType::EndGroup => {
            return Err(BqError::invalid_argument("Proto groups are not supported"));
        }
    }
    Ok(())
}

fn coerce_proto_value(val: BqValue, target: &BqType) -> BqResult<BqValue> {
    if val.is_null() {
        return Ok(BqValue::Null);
    }
    match (target, &val) {
        (BqType::Date, BqValue::Int64(days)) => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
            epoch
                .checked_add_signed(Duration::days(*days))
                .map(BqValue::Date)
                .ok_or_else(|| BqError::semantic("Proto DATE out of range"))
        }
        (BqType::Timestamp, BqValue::Int64(micros)) => DateTime::from_timestamp_micros(*micros)
            .map(BqValue::Timestamp)
            .ok_or_else(|| BqError::semantic("Proto TIMESTAMP out of range")),
        (BqType::Array(inner), BqValue::Array(items)) => {
            let coerced = items
                .iter()
                .map(|el| coerce_proto_value(el.clone(), inner))
                .collect::<BqResult<Vec<_>>>()?;
            Ok(BqValue::Array(coerced))
        }
        (BqType::Struct(fields), BqValue::Struct(svals)) => coerce_proto_struct(fields, svals),
        _ => cast_value(val, target),
    }
}

fn coerce_proto_struct(fields: &[StructField], svals: &[BqStructFieldValue]) -> BqResult<BqValue> {
    let mut out = Vec::with_capacity(fields.len());
    for sf in fields {
        let fname = sf.name.as_deref().unwrap_or("");
        let found = svals
            .iter()
            .find(|fv| {
                fv.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(fname))
            })
            .map_or(BqValue::Null, |fv| fv.value.clone());
        out.push(BqStructFieldValue {
            name: sf.name.clone(),
            value: coerce_proto_value(found, &sf.ty)?,
        });
    }
    Ok(BqValue::Struct(out))
}
