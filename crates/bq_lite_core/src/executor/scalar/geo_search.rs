use crate::error::{BqError, BqResult};
use crate::executor::context::ExecContext;
use crate::types::BqValue;

pub(super) const GEO_SEARCH_FUNCTIONS: &[&str] = &[
    "SEARCH",
    "SESSION_USER",
    "ST_ASTEXT",
    "ST_CONTAINS",
    "ST_DISTANCE",
    "ST_GEOGFROMTEXT",
    "ST_GEOGPOINT",
    "ST_WITHIN",
    "ST_X",
    "ST_Y",
];

pub(super) fn eval_geo_search_fn(
    name: &str,
    args: Vec<BqValue>,
    ctx: &ExecContext<'_>,
) -> BqResult<Option<BqValue>> {
    match name {
        "SEARCH" => eval_search(args).map(Some),
        "SESSION_USER" => {
            if !args.is_empty() {
                return Err(BqError::query("SESSION_USER expects 0 arguments"));
            }
            Ok(Some(BqValue::String(ctx.governance.session_user.clone())))
        }
        "ST_GEOGPOINT" => eval_st_geogpoint(args).map(Some),
        "ST_GEOGFROMTEXT" => eval_st_geogfromtext(args).map(Some),
        "ST_ASTEXT" => eval_st_astext(args).map(Some),
        "ST_DISTANCE" => eval_st_distance(args).map(Some),
        "ST_CONTAINS" => eval_st_contains(args, false).map(Some),
        "ST_WITHIN" => eval_st_contains(args, true).map(Some),
        "ST_X" => eval_st_xy(args, true).map(Some),
        "ST_Y" => eval_st_xy(args, false).map(Some),
        _ => Ok(None),
    }
}

fn eval_st_xy(args: Vec<BqValue>, is_x: bool) -> BqResult<BqValue> {
    let fname = if is_x { "ST_X" } else { "ST_Y" };
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query(format!("{fname} expects 1 argument")))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Geography(wkt) = arg else {
        return Err(BqError::query(format!("{fname} expects GEOGRAPHY")));
    };
    let (x, y) = parse_wkt_point(&wkt)?;
    Ok(BqValue::Float64(if is_x { x } else { y }))
}

fn eval_st_geogpoint(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [lon_v, lat_v]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("ST_GEOGPOINT expects 2 arguments"))?;
    if lon_v.is_null() || lat_v.is_null() {
        return Ok(BqValue::Null);
    }
    let lon = as_f64(&lon_v)?;
    let lat = as_f64(&lat_v)?;
    if !(-90.0..=90.0).contains(&lat) {
        return Err(BqError::semantic("Latitude must be in [-90, 90]"));
    }
    Ok(BqValue::Geography(format!("POINT({lon} {lat})")))
}

fn eval_st_geogfromtext(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ST_GEOGFROMTEXT expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(wkt) = arg else {
        return Err(BqError::query("ST_GEOGFROMTEXT expects STRING"));
    };
    if wkt.trim().is_empty() {
        return Err(BqError::semantic("Empty WKT string"));
    }
    Ok(BqValue::Geography(wkt.trim().to_owned()))
}

fn eval_st_astext(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [arg]: [BqValue; 1] = args
        .try_into()
        .map_err(|_| BqError::query("ST_ASTEXT expects 1 argument"))?;
    if arg.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::Geography(g) = arg else {
        return Err(BqError::query("ST_ASTEXT expects GEOGRAPHY"));
    };
    Ok(BqValue::String(g))
}

fn parse_wkt_point(wkt: &str) -> BqResult<(f64, f64)> {
    let inner = wkt
        .trim()
        .strip_prefix("POINT(")
        .or_else(|| wkt.trim().strip_prefix("point("))
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| BqError::semantic(format!("Expected POINT(lon lat), got '{wkt}'")))?;
    let mut nums = inner.split_whitespace();
    let lon = nums
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| BqError::semantic("Invalid longitude in POINT"))?;
    let lat = nums
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| BqError::semantic("Invalid latitude in POINT"))?;
    Ok((lon, lat))
}

fn eval_st_distance(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [a_v, b_v]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("ST_DISTANCE expects 2 arguments"))?;
    if a_v.is_null() || b_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Geography(ga), BqValue::Geography(gb)) = (a_v, b_v) else {
        return Err(BqError::query("ST_DISTANCE expects (GEOGRAPHY, GEOGRAPHY)"));
    };
    let (lon1, lat1) = parse_wkt_point(&ga)?;
    let (lon2, lat2) = parse_wkt_point(&gb)?;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let rlat1 = lat1.to_radians();
    let rlat2 = lat2.to_radians();
    let sin_lat = (dlat / 2.0).sin();
    let sin_lon = (dlon / 2.0).sin();
    let a = sin_lat.mul_add(sin_lat, rlat1.cos() * rlat2.cos() * sin_lon * sin_lon);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    Ok(BqValue::Float64(6_371_010.0 * c))
}

fn eval_st_contains(args: Vec<BqValue>, within: bool) -> BqResult<BqValue> {
    let [a_v, b_v]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("ST_CONTAINS/WITHIN expects 2 arguments"))?;
    if a_v.is_null() || b_v.is_null() {
        return Ok(BqValue::Null);
    }
    let (BqValue::Geography(ga), BqValue::Geography(gb)) = (a_v, b_v) else {
        return Err(BqError::query("ST_CONTAINS/WITHIN expects GEOGRAPHY"));
    };
    let (outer, inner) = if within { (&gb, &ga) } else { (&ga, &gb) };
    if outer.eq_ignore_ascii_case(inner) {
        return Ok(BqValue::Bool(true));
    }
    if let (Some(poly), Ok(pt)) = (parse_wkt_polygon(outer), parse_wkt_point(inner)) {
        return Ok(BqValue::Bool(point_in_polygon(pt, &poly)));
    }
    if let (Some(line), Ok(pt)) = (parse_wkt_linestring(outer), parse_wkt_point(inner)) {
        return Ok(BqValue::Bool(point_on_linestring(pt, &line)));
    }
    Ok(BqValue::Bool(false))
}

fn parse_wkt_polygon(wkt: &str) -> Option<Vec<(f64, f64)>> {
    let t = wkt.trim();
    let upper = t.to_ascii_uppercase();
    let rest = upper.strip_prefix("POLYGON")?;
    let start = t.len() - rest.len();
    let inner = t
        .get(start..)?
        .trim()
        .strip_prefix("((")?
        .strip_suffix("))")?;
    let outer_ring = inner.split("),(").next().unwrap_or(inner);
    parse_coord_list(outer_ring)
}

fn parse_wkt_linestring(wkt: &str) -> Option<Vec<(f64, f64)>> {
    let t = wkt.trim();
    let upper = t.to_ascii_uppercase();
    let rest = upper.strip_prefix("LINESTRING")?;
    let start = t.len() - rest.len();
    let inner = t
        .get(start..)?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    parse_coord_list(inner)
}

fn parse_coord_list(raw: &str) -> Option<Vec<(f64, f64)>> {
    let mut pts = Vec::new();
    for pair in raw.split(',') {
        let mut nums = pair.split_whitespace();
        let x = nums.next()?.parse::<f64>().ok()?;
        let y = nums.next()?.parse::<f64>().ok()?;
        pts.push((x, y));
    }
    (!pts.is_empty()).then_some(pts)
}

fn point_in_polygon((px, py): (f64, f64), ring: &[(f64, f64)]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = ring.len() - 1;
    for (i, &(xi, yi)) in ring.iter().enumerate() {
        let (xj, yj) = ring.get(j).copied().unwrap_or((xi, yi));
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn point_on_linestring((px, py): (f64, f64), line: &[(f64, f64)]) -> bool {
    const EPS: f64 = 1e-9;
    for win in line.windows(2) {
        if let [(x1, y1), (x2, y2)] = win {
            let cross = f64::mul_add(py - y1, -(x2 - x1), (px - x1) * (y2 - y1));
            let within_x = px >= x1.min(*x2) - EPS && px <= x1.max(*x2) + EPS;
            let within_y = py >= y1.min(*y2) - EPS && py <= y1.max(*y2) + EPS;
            if cross.abs() <= EPS && within_x && within_y {
                return true;
            }
        }
    }
    false
}

fn eval_search(args: Vec<BqValue>) -> BqResult<BqValue> {
    let [data, query]: [BqValue; 2] = args
        .try_into()
        .map_err(|_| BqError::query("SEARCH expects 2 arguments"))?;
    if data.is_null() || query.is_null() {
        return Ok(BqValue::Null);
    }
    let BqValue::String(qstr) = query else {
        return Err(BqError::query("SEARCH query must be STRING"));
    };
    let tokens: Vec<String> = qstr
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if tokens.is_empty() {
        return Ok(BqValue::Bool(false));
    }
    let mut hay = String::new();
    collect_search_text(&data, &mut hay);
    let hay_lower = hay.to_ascii_lowercase();
    Ok(BqValue::Bool(tokens.iter().all(|t| hay_lower.contains(t))))
}

fn collect_search_text(val: &BqValue, out: &mut String) {
    match val {
        BqValue::String(s) => {
            out.push(' ');
            out.push_str(s);
        }
        BqValue::Array(items) => {
            for item in items {
                collect_search_text(item, out);
            }
        }
        BqValue::Struct(fields) => {
            for f in fields {
                collect_search_text(&f.value, out);
            }
        }
        BqValue::Json(j) => {
            out.push(' ');
            out.push_str(&j.to_string());
        }
        BqValue::Null
        | BqValue::Bool(_)
        | BqValue::Int64(_)
        | BqValue::Float64(_)
        | BqValue::Numeric(_)
        | BqValue::BigNumeric(_)
        | BqValue::Bytes(_)
        | BqValue::Date(_)
        | BqValue::Datetime(_)
        | BqValue::Time(_)
        | BqValue::Timestamp(_)
        | BqValue::Interval(_)
        | BqValue::Geography(_)
        | BqValue::Range(_) => {
            out.push(' ');
            out.push_str(&val.to_string());
        }
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::wildcard_enum_match_arm,
    reason = "ST_GEOGPOINT accepts numeric coordinates"
)]
fn as_f64(v: &BqValue) -> BqResult<f64> {
    match v {
        BqValue::Float64(f) => Ok(*f),
        BqValue::Int64(n) => Ok(*n as f64),
        _ => Err(BqError::query("Expected numeric coordinate")),
    }
}
