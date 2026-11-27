# GoogleSQL Function Parity Reference

This document is **code-verified** by `crates/bq_lite_core/tests/googlesql_function_parity_doc.rs`.
Every function listed as `implemented` in a table below must exist in the corresponding live engine registry, and every registered engine function must be documented here.

## Scalar Functions

| Function | Status | Notes |
|---|---|---|
| `ABS` | `implemented` | Absolute value |
| `ARRAY_CONCAT` | `implemented` | Concatenates arrays |
| `ARRAY_FILTER` | `implemented` | Filters array via lambda predicate |
| `ARRAY_FIRST` | `implemented` | First element or error on empty |
| `ARRAY_LAST` | `implemented` | Last element or error on empty |
| `ARRAY_LENGTH` | `implemented` | Length of `ARRAY` |
| `ARRAY_REVERSE` | `implemented` | Reverses elements |
| `ARRAY_SLICE` | `implemented` | Slices `[start, end]` inclusive |
| `ARRAY_TO_STRING` | `implemented` | Joins array elements |
| `ARRAY_TRANSFORM` | `implemented` | Maps array elements via lambda |
| `ASCII` | `implemented` | ASCII code of first byte |
| `BOOL` | `implemented` | Extracts `BOOL` from `JSON` |
| `BYTE_LENGTH` | `implemented` | UTF-8 byte count |
| `CBRT` | `implemented` | Cube root |
| `CEIL` | `implemented` | Ceiling |
| `CEILING` | `implemented` | Alias for `CEIL` |
| `CHAR_LENGTH` | `implemented` | Character count |
| `COALESCE` | `implemented` | First non-`NULL` argument |
| `CODE_POINTS_TO_BYTES` | `implemented` | Converts `ARRAY<INT64>` byte code points to `BYTES` |
| `CODE_POINTS_TO_STRING` | `implemented` | Converts `ARRAY<INT64>` Unicode code points to `STRING` |
| `CONCAT` | `implemented` | `STRING` or `BYTES` concatenation |
| `CURRENT_DATE` | `implemented` | Current date (optional timezone) |
| `CURRENT_DATETIME` | `implemented` | Current UTC datetime |
| `CURRENT_TIME` | `implemented` | Current UTC time |
| `CURRENT_TIMESTAMP` | `implemented` | Current UTC timestamp |
| `DATE` | `implemented` | `DATE` constructor |
| `DATE_ADD` | `implemented` | Adds `INTERVAL` to `DATE` |
| `DATE_BUCKET` | `implemented` | Buckets `DATE` by `INTERVAL` width |
| `DATE_DIFF` | `implemented` | Difference between `DATE`s |
| `DATE_FROM_UNIX_DATE` | `implemented` | Epoch days to `DATE` |
| `DATE_SUB` | `implemented` | Subtracts `INTERVAL` from `DATE` |
| `DATE_TRUNC` | `implemented` | Truncates `DATE` to part |
| `DATETIME` | `implemented` | `DATETIME` constructor |
| `DATETIME_ADD` | `implemented` | Adds `INTERVAL` to `DATETIME` |
| `DATETIME_BUCKET` | `implemented` | Buckets `DATETIME` by `INTERVAL` width |
| `DATETIME_DIFF` | `implemented` | Difference between `DATETIME`s |
| `DATETIME_SUB` | `implemented` | Subtracts `INTERVAL` from `DATETIME` |
| `DATETIME_TRUNC` | `implemented` | Truncates `DATETIME` to part |
| `DIV` | `implemented` | Integer division |
| `ENDS_WITH` | `implemented` | Suffix check |
| `ERROR` | `implemented` | Raises semantic error |
| `EXP` | `implemented` | Exponential `e^x` |
| `FARM_FINGERPRINT` | `implemented` | Exact `FarmHash` `Fingerprint64` |
| `FLOAT64` | `implemented` | Extracts `FLOAT64` from `JSON` |
| `FLOOR` | `implemented` | Floor |
| `FORMAT` | `implemented` | Printf-style formatting |
| `FORMAT_DATE` | `implemented` | Formats `DATE` |
| `FORMAT_DATETIME` | `implemented` | Formats `DATETIME` |
| `FORMAT_TIME` | `implemented` | Formats `TIME` |
| `FORMAT_TIMESTAMP` | `implemented` | Formats `TIMESTAMP` |
| `FROM_BASE64` | `implemented` | Base64 decode to `BYTES` |
| `FROM_HEX` | `implemented` | Hex decode to `BYTES` |
| `GENERATE_ARRAY` | `implemented` | Integer sequence array |
| `GENERATE_DATE_ARRAY` | `implemented` | Date sequence array |
| `GENERATE_UUID` | `implemented` | Random UUID v4 |
| `GREATEST` | `implemented` | Maximum of arguments |
| `IEEE_DIVIDE` | `implemented` | IEEE-754 division |
| `IF` | `implemented` | Conditional expression |
| `IFNULL` | `implemented` | Null fallback |
| `INITCAP` | `implemented` | Title case |
| `INSTR` | `implemented` | Substring search |
| `INT64` | `implemented` | Extracts `INT64` from `JSON` |
| `JSON_ARRAY` | `implemented` | Builds `JSON` array |
| `JSON_EXTRACT` | `implemented` | Legacy JSON extract |
| `JSON_EXTRACT_ARRAY` | `implemented` | Legacy JSON array extract |
| `JSON_EXTRACT_SCALAR` | `implemented` | Legacy JSON scalar extract |
| `JSON_EXTRACT_STRING_ARRAY` | `implemented` | Legacy JSON string array extract |
| `JSON_KEYS` | `implemented` | Top-level keys of JSON object |
| `JSON_OBJECT` | `implemented` | Builds `JSON` object |
| `JSON_QUERY` | `implemented` | JSON fragment query |
| `JSON_QUERY_ARRAY` | `implemented` | JSON array fragment query |
| `JSON_REMOVE` | `implemented` | Removes JSONPath targets |
| `JSON_SET` | `implemented` | Sets JSONPath targets |
| `JSON_TYPE` | `implemented` | JSON value type name |
| `JSON_VALUE` | `implemented` | JSON scalar extraction |
| `JSON_VALUE_ARRAY` | `implemented` | JSON scalar array extraction |
| `JUSTIFY_DAYS` | `implemented` | Normalizes 30-day chunks into months |
| `JUSTIFY_HOURS` | `implemented` | Normalizes 24-hour chunks into days |
| `JUSTIFY_INTERVAL` | `implemented` | Normalizes hours, days, and months |
| `LAST_DAY` | `implemented` | Last day of month |
| `LEAST` | `implemented` | Minimum of arguments |
| `LEFT` | `implemented` | Left prefix characters or bytes |
| `LENGTH` | `implemented` | Character or byte length |
| `LN` | `implemented` | Natural logarithm |
| `LOG` | `implemented` | Logarithm |
| `LOG10` | `implemented` | Base-10 logarithm |
| `LOWER` | `implemented` | Lowercase |
| `LPAD` | `implemented` | Left pad (`STRING` or `BYTES`) |
| `LTRIM` | `implemented` | Left trim (`STRING` or `BYTES`) |
| `MAKE_INTERVAL` | `implemented` | Constructs `INTERVAL` from date/time components |
| `MD5` | `implemented` | MD5 digest (`BYTES`) |
| `MOD` | `implemented` | Integer modulo |
| `NET.HOST` | `implemented` | Extracts host from URL |
| `NET.IP_FROM_STRING` | `implemented` | Parses IPv4/IPv6 string to `BYTES` |
| `NET.IP_TO_STRING` | `implemented` | Formats IP `BYTES` as string |
| `NET.PUBLIC_SUFFIX` | `implemented` | Extracts public suffix |
| `NET.REG_DOMAIN` | `implemented` | Extracts registrable domain |
| `NET.SAFE_IP_FROM_STRING` | `implemented` | Null-safe IP string parser |
| `NULLIF` | `implemented` | Returns `NULL` if equal |
| `OCTET_LENGTH` | `implemented` | Byte length of `STRING` or `BYTES` |
| `PARSE_DATE` | `implemented` | Parses `DATE` |
| `PARSE_DATETIME` | `implemented` | Parses `DATETIME` |
| `PARSE_JSON` | `implemented` | Parses JSON string |
| `PARSE_TIME` | `implemented` | Parses `TIME` |
| `PARSE_TIMESTAMP` | `implemented` | Parses `TIMESTAMP` |
| `POW` | `implemented` | Power |
| `POWER` | `implemented` | Alias for `POW` |
| `RAND` | `implemented` | Uniform random `[0, 1)` |
| `RANGE` | `implemented` | Constructs `RANGE<T>` |
| `RANGE_BUCKET` | `implemented` | Histogram bucket index |
| `RANGE_CONTAINS` | `implemented` | Checks point or subrange containment |
| `RANGE_END` | `implemented` | Exclusive upper bound of `RANGE` |
| `RANGE_INTERSECT` | `implemented` | Intersection of overlapping `RANGE`s |
| `RANGE_OVERLAPS` | `implemented` | Overlap check for `RANGE`s |
| `RANGE_START` | `implemented` | Inclusive lower bound of `RANGE` |
| `REGEXP_CONTAINS` | `implemented` | Regex match test |
| `REGEXP_EXTRACT` | `implemented` | Regex group extract |
| `REGEXP_EXTRACT_ALL` | `implemented` | All regex matches |
| `REGEXP_INSTR` | `implemented` | Regex 1-based position |
| `REGEXP_REPLACE` | `implemented` | Regex replacement |
| `REGEXP_SUBSTR` | `implemented` | Alias for `REGEXP_EXTRACT` |
| `REPEAT` | `implemented` | Repeats string |
| `REPLACE` | `implemented` | Substring replacement |
| `REVERSE` | `implemented` | Reverses string |
| `RIGHT` | `implemented` | Right suffix characters |
| `ROUND` | `implemented` | Rounds float |
| `RPAD` | `implemented` | Right pad |
| `RTRIM` | `implemented` | Right trim |
| `SAFE_ADD` | `implemented` | Overflow-safe addition |
| `SAFE_CONVERT_BYTES_TO_STRING` | `implemented` | Converts `BYTES` to `STRING` replacing invalid UTF-8 with `U+FFFD` |
| `SAFE_DIVIDE` | `implemented` | Divide-by-zero-safe division |
| `SAFE_MULTIPLY` | `implemented` | Overflow-safe multiplication |
| `SAFE_NEGATE` | `implemented` | Overflow-safe negation |
| `SAFE_SUBTRACT` | `implemented` | Overflow-safe subtraction |
| `SEARCH` | `implemented` | Tokenized search over `STRING`/`ARRAY`/`STRUCT`/`JSON` |
| `SESSION_USER` | `implemented` | Current session principal email |
| `SHA1` | `implemented` | SHA-1 digest (`BYTES`) |
| `SHA256` | `implemented` | SHA-256 digest (`BYTES`) |
| `SHA512` | `implemented` | SHA-512 digest (`BYTES`) |
| `SIGN` | `implemented` | Sign (`-1`, `0`, `1`) |
| `SPLIT` | `implemented` | Splits string into `ARRAY` |
| `SQRT` | `implemented` | Square root |
| `ST_ASTEXT` | `implemented` | Converts `GEOGRAPHY` to WKT |
| `ST_CONTAINS` | `implemented` | Geography containment check |
| `ST_DISTANCE` | `implemented` | Spherical distance in meters |
| `ST_GEOGFROMTEXT` | `implemented` | Parses WKT into `GEOGRAPHY` |
| `ST_GEOGPOINT` | `implemented` | Constructs `GEOGRAPHY` point |
| `ST_WITHIN` | `implemented` | Geography within check |
| `STARTS_WITH` | `implemented` | Prefix check |
| `STRING` | `implemented` | Extracts `STRING` from `JSON` |
| `STRPOS` | `implemented` | 1-based substring position |
| `SUBSTR` | `implemented` | Substring |
| `SUBSTRING` | `implemented` | Alias for `SUBSTR` |
| `TIME` | `implemented` | `TIME` constructor |
| `TIME_ADD` | `implemented` | Adds `INTERVAL` to `TIME` |
| `TIME_DIFF` | `implemented` | Difference between `TIME`s |
| `TIME_SUB` | `implemented` | Subtracts `INTERVAL` from `TIME` |
| `TIME_TRUNC` | `implemented` | Truncates `TIME` to part |
| `TIMESTAMP` | `implemented` | `TIMESTAMP` constructor |
| `TIMESTAMP_ADD` | `implemented` | Adds `INTERVAL` to `TIMESTAMP` |
| `TIMESTAMP_BUCKET` | `implemented` | Buckets `TIMESTAMP` by `INTERVAL` width |
| `TIMESTAMP_DIFF` | `implemented` | Difference between `TIMESTAMP`s |
| `TIMESTAMP_MICROS` | `implemented` | Microseconds since epoch |
| `TIMESTAMP_MILLIS` | `implemented` | Milliseconds since epoch |
| `TIMESTAMP_SECONDS` | `implemented` | Seconds since epoch |
| `TIMESTAMP_SUB` | `implemented` | Subtracts `INTERVAL` from `TIMESTAMP` |
| `TIMESTAMP_TRUNC` | `implemented` | Truncates `TIMESTAMP` to part |
| `TO_BASE64` | `implemented` | Encodes `BYTES` as base64 |
| `TO_CODE_POINTS` | `implemented` | Converts `STRING`/`BYTES` to `ARRAY<INT64>` code points |
| `TO_HEX` | `implemented` | Encodes `BYTES` as hex |
| `TO_JSON` | `implemented` | Converts value to `JSON` |
| `TO_JSON_STRING` | `implemented` | Converts value to JSON string (preserves `STRUCT` field order) |
| `TRANSLATE` | `implemented` | Character translation |
| `TRIM` | `implemented` | Whitespace/char trim |
| `TRUNC` | `implemented` | Truncates float/numeric |
| `UNICODE` | `implemented` | Unicode code point of first character |
| `UNIX_DATE` | `implemented` | Days since 1970-01-01 |
| `UNIX_MICROS` | `implemented` | Epoch microseconds |
| `UNIX_MILLIS` | `implemented` | Epoch milliseconds |
| `UNIX_SECONDS` | `implemented` | Epoch seconds |
| `UPPER` | `implemented` | Uppercase |

## Aggregate Functions

| Function | Status | Notes |
|---|---|---|
| `ANY_VALUE` | `implemented` | Supports `HAVING MAX` / `HAVING MIN` |
| `APPROX_COUNT_DISTINCT` | `implemented` | Approximate distinct count |
| `APPROX_QUANTILES` | `implemented` | Approximate quantile array (`NULL` on empty input) |
| `ARRAY_AGG` | `implemented` | Supports `DISTINCT`, `IGNORE NULLS`, `ORDER BY`, `LIMIT` |
| `AVG` | `implemented` | Numeric average |
| `BIT_AND` | `implemented` | Bitwise AND aggregate |
| `BIT_OR` | `implemented` | Bitwise OR aggregate |
| `BIT_XOR` | `implemented` | Bitwise XOR aggregate |
| `COUNT` | `implemented` | Supports `*` and `DISTINCT` |
| `COUNTIF` | `implemented` | Conditional count |
| `LOGICAL_AND` | `implemented` | Boolean AND aggregate |
| `LOGICAL_OR` | `implemented` | Boolean OR aggregate |
| `MAX` | `implemented` | Maximum non-`NULL` value |
| `MIN` | `implemented` | Minimum non-`NULL` value |
| `STDDEV` | `implemented` | Sample standard deviation |
| `STDDEV_POP` | `implemented` | Population standard deviation |
| `STDDEV_SAMP` | `implemented` | Sample standard deviation |
| `STRING_AGG` | `implemented` | Supports `DISTINCT` and `ORDER BY` |
| `SUM` | `implemented` | Numeric sum with supertype promotion |
| `VAR_POP` | `implemented` | Population variance |
| `VAR_SAMP` | `implemented` | Sample variance |
| `VARIANCE` | `implemented` | Sample variance |

## Window Functions

| Function | Status | Notes |
|---|---|---|
| `CUME_DIST` | `implemented` | Cumulative distribution |
| `DENSE_RANK` | `implemented` | Dense ranking without gaps |
| `FIRST_VALUE` | `implemented` | Supports `IGNORE/RESPECT NULLS` and frames |
| `LAG` | `implemented` | Supports offset, default, and `IGNORE NULLS` |
| `LAST_VALUE` | `implemented` | Supports `IGNORE/RESPECT NULLS` and frames |
| `LEAD` | `implemented` | Supports offset, default, and `IGNORE NULLS` |
| `NTH_VALUE` | `implemented` | Supports `IGNORE/RESPECT NULLS` and frames |
| `NTILE` | `implemented` | Bucket partitioning |
| `PERCENT_RANK` | `implemented` | Relative peer rank |
| `RANK` | `implemented` | Ranking with gaps |
| `ROW_NUMBER` | `implemented` | Sequential 1-based row number |

## Table-Valued Functions (TVFs)

| Function | Status | Notes |
|---|---|---|
