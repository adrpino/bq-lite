mod datasets;
mod gcs;
mod jobs_copy;
mod jobs_queries;
mod load_arrow;
mod params;
mod routines;
mod tabledata;
mod tables;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::CorsLayer;

use crate::state::ServerState;

pub fn build_router(state: ServerState) -> Router {
    let project_routes = build_project_routes();
    Router::new()
        .nest("/bigquery/v2", project_routes.clone())
        .merge(project_routes)
        .route(
            "/upload/bigquery/v2/projects/{projectId}/jobs",
            post(load_arrow::upload_load_job).put(load_arrow::put_resumable_upload),
        )
        .route(
            "/upload/projects/{projectId}/jobs",
            post(load_arrow::upload_load_job).put(load_arrow::put_resumable_upload),
        )
        .route(
            "/_bqlite/arrow/{jobId}",
            get(load_arrow::download_arrow_ipc),
        )
        .route("/token", post(mock_oauth_token))
        .route("/oauth2/v4/token", post(mock_oauth_token))
        .route("/discovery/v1/apis/bigquery/v2/rest", get(discovery_stub))
        .route("/healthz", get(healthz))
        .route("/health", get(healthz))
        .route("/_bqlite/reset", post(reset_state))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn build_project_routes() -> Router<ServerState> {
    Router::new()
        .route("/projects", get(datasets::list_projects))
        .route(
            "/projects/{projectId}/datasets",
            get(datasets::list_datasets).post(datasets::insert_dataset),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}",
            get(datasets::get_dataset)
                .patch(datasets::patch_dataset)
                .put(datasets::put_dataset)
                .delete(datasets::delete_dataset),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/tables",
            get(tables::list_tables).post(tables::insert_table),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/tables/{tableId}",
            get(tables::get_table)
                .patch(tables::patch_table)
                .put(tables::put_table)
                .delete(tables::delete_table),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/tables/{tableId}/data",
            get(tabledata::list_tabledata),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/tables/{tableId}/insertAll",
            post(tabledata::insert_all),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/routines",
            get(routines::list_routines).post(routines::insert_routine),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/routines/{routineId}",
            get(routines::get_routine)
                .put(routines::put_routine)
                .delete(routines::delete_routine),
        )
        .route(
            "/projects/{projectId}/datasets/{datasetId}/models",
            get(empty_models_list),
        )
        .route(
            "/projects/{projectId}/queries",
            post(jobs_queries::sync_query),
        )
        .route(
            "/projects/{projectId}/queries/{jobId}",
            get(jobs_queries::get_query_results),
        )
        .route(
            "/projects/{projectId}/jobs",
            get(jobs_queries::list_jobs).post(jobs_queries::insert_job),
        )
        .route(
            "/projects/{projectId}/jobs/{jobId}",
            get(jobs_queries::get_job),
        )
        .route(
            "/projects/{projectId}/jobs/{jobId}/cancel",
            post(jobs_queries::cancel_job),
        )
}

async fn healthz() -> Response {
    Json(json!({ "status": "ok" })).into_response()
}

async fn reset_state(State(state): State<ServerState>) -> Response {
    state.reset();
    StatusCode::NO_CONTENT.into_response()
}

async fn mock_oauth_token() -> Response {
    Json(json!({
        "access_token": "bq-lite-mock-token",
        "token_type": "Bearer",
        "expires_in": 3600,
    }))
    .into_response()
}

async fn discovery_stub() -> Response {
    Json(json!({
        "kind": "discovery#restDescription",
        "name": "bigquery",
        "version": "v2",
    }))
    .into_response()
}

async fn empty_models_list() -> Response {
    Json(json!({ "models": [] })).into_response()
}
