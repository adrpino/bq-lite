#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CACHE_DIR="${BQ_LITE_SDK_CACHE:-/tmp/bq-lite-sdk-cache}"
mkdir -p "$CACHE_DIR"

echo "==> Building bq-lite-server"
cargo build -p bq-lite-server

PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
GRPC_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
LOG_FILE="$CACHE_DIR/server.log"

"$ROOT_DIR/target/debug/bq-lite-server" --port "$PORT" --grpc-port "$GRPC_PORT" --project test-proj --memory >"$LOG_FILE" 2>&1 &
SERVER_PID=$!

cleanup() {
    if kill -0 "$SERVER_PID" >/dev/null 2>&1; then
        kill "$SERVER_PID" >/dev/null 2>&1 || true
        wait "$SERVER_PID" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

for _ in $(seq 1 50); do
    if curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null

echo "==> 1. Python google-cloud-bigquery SDK E2E"
if command -v python3 >/dev/null 2>&1; then
    PY_VENV="$CACHE_DIR/py_venv"
    if [ ! -x "$PY_VENV/bin/python" ]; then
        python3 -m venv "$PY_VENV"
        "$PY_VENV/bin/pip" install --quiet --upgrade pip
    fi
    if ! "$PY_VENV/bin/python" -c "import google.cloud.bigquery, google.cloud.bigquery_storage, pyarrow" >/dev/null 2>&1; then
        "$PY_VENV/bin/pip" install --quiet google-cloud-bigquery google-cloud-bigquery-storage pyarrow
    fi
    BQ_ENDPOINT="http://127.0.0.1:$PORT" BQ_GRPC_ENDPOINT="127.0.0.1:$GRPC_PORT" "$PY_VENV/bin/python" - <<'PY'
import io
import os
from decimal import Decimal
from google.auth.credentials import AnonymousCredentials
from google.cloud import bigquery

client = bigquery.Client(
    project="test-proj",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": os.environ["BQ_ENDPOINT"]},
)

ds_ref = bigquery.DatasetReference("test-proj", "sdk_py")
client.create_dataset(bigquery.Dataset(ds_ref), exists_ok=True)
client.create_dataset(bigquery.Dataset(ds_ref), exists_ok=True)

schema = [
    bigquery.SchemaField("id", "INT64", mode="REQUIRED"),
    bigquery.SchemaField("amount", "NUMERIC"),
    bigquery.SchemaField("payload", "BYTES"),
    bigquery.SchemaField("tags", "STRING", mode="REPEATED"),
    bigquery.SchemaField("meta", "RECORD", fields=[
        bigquery.SchemaField("label", "STRING"),
    ]),
]
tbl = client.create_table(bigquery.Table(ds_ref.table("orders"), schema=schema))

errs = client.insert_rows_json(tbl, [
    {"id": 1, "amount": "19.99", "payload": "aGk=", "tags": ["a", "b"], "meta": {"label": "first"}},
    {"id": 2, "amount": "5.01", "payload": "Ynll", "tags": [], "meta": {"label": "second"}},
])
assert errs == [], f"insert_rows_json errors: {errs}"

rows = list(client.list_rows(tbl, max_results=1))
assert len(rows) == 1

q_cfg = bigquery.QueryJobConfig(
    query_parameters=[
        bigquery.ScalarQueryParameter("bonus", "NUMERIC", Decimal("1.50")),
        bigquery.ArrayQueryParameter("ids", "INT64", [1, 2]),
    ]
)
res = list(client.query(
    "SELECT id, amount + @bonus AS tot, payload, meta.label AS lbl FROM sdk_py.orders WHERE id IN UNNEST(@ids) ORDER BY id",
    job_config=q_cfg,
).result())
assert len(res) == 2
assert res[0]["tot"] == Decimal("21.49")
assert res[0]["payload"] == b"hi"
assert res[0]["lbl"] == "first"

dry_job = client.query(
    "SELECT id, amount FROM sdk_py.orders",
    job_config=bigquery.QueryJobConfig(dry_run=True, use_query_cache=False),
)
assert [f.name for f in dry_job.schema] == ["id", "amount"]

# CSV load
csv_tbl = ds_ref.table("csv_items")
csv_job = client.load_table_from_file(
    io.BytesIO(b"id,name\n10,ada\n20,grace\n"),
    csv_tbl,
    job_config=bigquery.LoadJobConfig(
        source_format=bigquery.SourceFormat.CSV,
        skip_leading_rows=1,
        schema=[
            bigquery.SchemaField("id", "INT64"),
            bigquery.SchemaField("name", "STRING"),
        ],
    ),
)
csv_job.result()
assert client.get_table(csv_tbl).num_rows == 2

# NDJSON load with REPEATED RECORD
ndjson_tbl = ds_ref.table("json_events")
ndjson_job = client.load_table_from_file(
    io.BytesIO(b'{"id":1,"ev":[{"k":"click"}]}\n'),
    ndjson_tbl,
    job_config=bigquery.LoadJobConfig(
        source_format=bigquery.SourceFormat.NEWLINE_DELIMITED_JSON,
        schema=[
            bigquery.SchemaField("id", "INT64"),
            bigquery.SchemaField("ev", "RECORD", mode="REPEATED", fields=[
                bigquery.SchemaField("k", "STRING"),
            ]),
        ],
    ),
)
ndjson_job.result()
assert client.get_table(ndjson_tbl).num_rows == 1

# Copy table
copy_job = client.copy_table(csv_tbl, ds_ref.table("csv_copy"))
copy_job.result()
assert client.get_table(ds_ref.table("csv_copy")).num_rows == 2

# gRPC BigQueryReadClient (Arrow IPC streaming)
import grpc
from google.cloud import bigquery_storage
from google.cloud.bigquery_storage_v1.services.big_query_read import transports as bqread_transports

grpc_channel = grpc.insecure_channel(os.environ["BQ_GRPC_ENDPOINT"])
read_transport = bqread_transports.BigQueryReadGrpcTransport(channel=grpc_channel)
bqstorage_client = bigquery_storage.BigQueryReadClient(transport=read_transport)
session = bqstorage_client.create_read_session(
    parent="projects/test-proj",
    read_session=bigquery_storage.types.ReadSession(
        table="projects/test-proj/datasets/sdk_py/tables/csv_copy",
        data_format=bigquery_storage.types.DataFormat.ARROW,
    ),
    max_stream_count=1,
)
reader = bqstorage_client.read_rows(session.streams[0].name)
arrow_tbl = reader.to_arrow(session)
assert arrow_tbl.num_rows == 2

# Parquet upload via load_table_from_file (binary multipart)
import pyarrow as pa
import pyarrow.parquet as pq

pq_buf = io.BytesIO()
pq_table = pa.table({"id": pa.array([10, 20], type=pa.int64()), "val": pa.array([1.5, 2.5], type=pa.float64())})
pq.write_table(pq_table, pq_buf)
pq_buf.seek(0)
pq_job = client.load_table_from_file(
    pq_buf,
    ds_ref.table("pq_items"),
    job_config=bigquery.LoadJobConfig(source_format=bigquery.SourceFormat.PARQUET),
)
pq_job.result()
assert client.get_table(ds_ref.table("pq_items")).num_rows == 2

# Auto-handoff query -> BigQueryReadClient via destinationTable
q_arrow = client.query("SELECT id, val FROM sdk_py.pq_items ORDER BY id").to_arrow(bqstorage_client=bqstorage_client)
assert q_arrow.num_rows == 2

# gRPC BigQueryWriteClient (_default stream + proto_rows)
from google.cloud.bigquery_storage_v1 import BigQueryWriteClient
from google.cloud.bigquery_storage_v1.services.big_query_write import transports as bqwrite_transports
from google.cloud.bigquery_storage_v1 import types as bq_storage_types
from google.protobuf import descriptor_pb2

write_transport = bqwrite_transports.BigQueryWriteGrpcTransport(channel=grpc_channel)
write_client = BigQueryWriteClient(transport=write_transport)
desc = descriptor_pb2.DescriptorProto(
    name="PqRow",
    field=[
        descriptor_pb2.FieldDescriptorProto(name="id", number=1, label=1, type=3),
        descriptor_pb2.FieldDescriptorProto(name="val", number=2, label=1, type=1),
    ],
)
# Wire bytes: field 1 varint 30 (0x08 0x1e) + field 2 double 3.5 (0x11 + 8 LE bytes)
import struct
row_wire = b"\x08\x1e\x11" + struct.pack("<d", 3.5)
append_req = bq_storage_types.AppendRowsRequest(
    write_stream="projects/test-proj/datasets/sdk_py/tables/pq_items/streams/_default",
    proto_rows=bq_storage_types.AppendRowsRequest.ProtoData(
        writer_schema=bq_storage_types.ProtoSchema(proto_descriptor=desc),
        rows=bq_storage_types.ProtoRows(serialized_rows=[row_wire]),
    ),
)
list(write_client.append_rows(iter([append_req])))
assert client.get_table(ds_ref.table("pq_items")).num_rows == 3

client.delete_table(ds_ref.table("csv_copy"))
client.delete_dataset(ds_ref, delete_contents=True)
print("Python google-cloud-bigquery SDK E2E PASSED")
PY
else
    echo "python3 not installed, skipping Python SDK"
fi

echo "==> 2. Node @google-cloud/bigquery SDK E2E"
if command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then
    NODE_DIR="$CACHE_DIR/node_sdk"
    if [ ! -d "$NODE_DIR/node_modules/@google-cloud/bigquery" ]; then
        mkdir -p "$NODE_DIR"
        npm --prefix "$NODE_DIR" install --silent --no-save @google-cloud/bigquery
    fi
    NODE_PATH="$NODE_DIR/node_modules" BQ_ENDPOINT="http://127.0.0.1:$PORT" node - <<'JS'
const assert = require('assert');
const { BigQuery } = require('@google-cloud/bigquery');

async function main() {
  const bq = new BigQuery({
    projectId: 'test-proj',
    apiEndpoint: process.env.BQ_ENDPOINT,
  });
  const [ds] = await bq.createDataset('sdk_node');
  const [exists] = await ds.exists();
  assert.strictEqual(exists, true);

  const [tbl] = await ds.createTable('events', {
    schema: [
      { name: 'id', type: 'INT64' },
      { name: 'name', type: 'STRING' },
    ],
  });
  await tbl.insert([
    { id: 1, name: 'alpha' },
    { id: 2, name: 'beta' },
  ]);

  const [rows] = await bq.query({
    query: 'SELECT id, name FROM sdk_node.events WHERE id >= @min_id ORDER BY id',
    params: { min_id: 1 },
  });
  assert.strictEqual(rows.length, 2);
  assert.strictEqual(Number(rows[0].id), 1);
  assert.strictEqual(rows[1].name, 'beta');

  await ds.delete({ force: true });
  console.log('Node @google-cloud/bigquery SDK E2E PASSED');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
JS
else
    echo "node/npm not installed, skipping Node SDK"
fi

echo "==> 3. Go cloud.google.com/go/bigquery SDK E2E"
if command -v go >/dev/null 2>&1; then
    GO_DIR="$CACHE_DIR/go_sdk"
    mkdir -p "$GO_DIR"
    cat >"$GO_DIR/go.mod" <<'GOMOD'
module bqlitesdktest

go 1.22

require (
	cloud.google.com/go/bigquery v1.64.0
	google.golang.org/api v0.214.0
	google.golang.org/grpc v1.69.2
)
GOMOD
    cat >"$GO_DIR/main.go" <<'GOCODE'
package main

import (
	"context"
	"fmt"
	"os"

	"cloud.google.com/go/bigquery"
	"google.golang.org/api/iterator"
	"google.golang.org/api/option"
)

func main() {
	ctx := context.Background()
	client, err := bigquery.NewClient(ctx, "test-proj",
		option.WithEndpoint(os.Getenv("BQ_ENDPOINT")+"/bigquery/v2/"),
		option.WithoutAuthentication(),
	)
	if err != nil {
		panic(err)
	}
	defer client.Close()

	ds := client.Dataset("sdk_go")
	if err := ds.Create(ctx, &bigquery.DatasetMetadata{Location: "US"}); err != nil {
		panic(err)
	}
	q := client.Query("SELECT 42 AS answer")
	it, err := q.Read(ctx)
	if err != nil {
		panic(err)
	}
	var row []bigquery.Value
	if err := it.Next(&row); err != nil && err != iterator.Done {
		panic(err)
	}
	if fmt.Sprint(row[0]) != "42" {
		panic(fmt.Sprintf("unexpected row: %v", row))
	}
	if err := ds.DeleteWithContents(ctx); err != nil {
		panic(err)
	}
	fmt.Println("Go cloud.google.com/go/bigquery SDK E2E PASSED")
}
GOCODE
    (cd "$GO_DIR" && go mod tidy -e >/dev/null 2>&1 || true)
    (cd "$GO_DIR" && BQ_ENDPOINT="http://127.0.0.1:$PORT" go run .)
else
    echo "go not installed, skipping Go SDK"
fi

echo "==> 4. Rust gcp-bigquery-client SDK E2E"
if command -v cargo >/dev/null 2>&1; then
    RUST_SDK_DIR="$CACHE_DIR/rust_sdk"
    mkdir -p "$RUST_SDK_DIR/src"
    cat >"$RUST_SDK_DIR/Cargo.toml" <<'RCARGO'
[package]
name = "bq-lite-rust-sdk-smoke"
version = "0.1.0"
edition = "2021"

[dependencies]
async-trait = "0.1"
gcp-bigquery-client = "0.28"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
RCARGO
    cat >"$RUST_SDK_DIR/src/main.rs" <<'RMAIN'
use std::sync::Arc;
use gcp_bigquery_client::auth::Authenticator;
use gcp_bigquery_client::client_builder::ClientBuilder;
use gcp_bigquery_client::error::BQError;
use gcp_bigquery_client::model::dataset::Dataset;
use gcp_bigquery_client::model::query_request::QueryRequest;
use gcp_bigquery_client::model::query_response::ResultSet;

#[derive(Clone)]
struct LocalAuth;

#[async_trait::async_trait]
impl Authenticator for LocalAuth {
    async fn access_token(&self) -> Result<String, BQError> {
        Ok("local-token".to_owned())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("BQ_ENDPOINT")?;
    let client = ClientBuilder::new()
        .with_v2_base_url(format!("{endpoint}/bigquery/v2"))
        .build_from_authenticator(Arc::new(LocalAuth))
        .await?;

    client
        .dataset()
        .create(Dataset::new("test-proj", "sdk_rust"))
        .await?;

    let resp = client
        .job()
        .query("test-proj", QueryRequest::new("SELECT 123 AS n, 'ok' AS s"))
        .await?;
    let mut rs = ResultSet::new_from_query_response(resp);
    assert!(rs.next_row());
    assert_eq!(rs.get_i64_by_name("n")?, Some(123));
    assert_eq!(rs.get_string_by_name("s")?.as_deref(), Some("ok"));

    client
        .dataset()
        .delete("test-proj", "sdk_rust", true)
        .await?;
    println!("Rust gcp-bigquery-client SDK E2E PASSED");
    Ok(())
}
RMAIN
    BQ_ENDPOINT="http://127.0.0.1:$PORT" cargo run --quiet --manifest-path "$RUST_SDK_DIR/Cargo.toml"
else
    echo "cargo not installed, skipping Rust SDK"
fi

echo "==> 5. dbt-bigquery advanced E2E suite"
if command -v python3 >/dev/null 2>&1; then
    BQ_LITE_ENDPOINT="http://127.0.0.1:$PORT" "$ROOT_DIR/scripts/test-dbt.sh"
fi

echo "==> SDK E2E suite PASSED"
