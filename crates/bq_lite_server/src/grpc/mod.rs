pub mod avro;
pub mod proto;
pub mod proto_rows;
pub mod read;
pub mod write;
mod write_commit;

use std::net::SocketAddr;

use tokio::net::TcpListener;
use tonic::service::Routes;

use crate::ServerState;
use read::BigQueryReadService;
use write::BigQueryWriteService;

/// Builds the combined `tonic` gRPC router for `BigQueryRead` and `BigQueryWrite`.
#[must_use]
pub fn build_grpc_router(state: ServerState) -> Routes {
    Routes::new(BigQueryReadService::new(state.clone()))
        .add_service(BigQueryWriteService::new(state))
        .prepare()
}

/// Serves the `tonic` gRPC `BigQuery` Storage API on `listener`.
pub async fn serve_grpc_with_listener(
    listener: TcpListener,
    state: ServerState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tonic::transport::Server::builder()
        .add_routes(build_grpc_router(state))
        .serve_with_incoming(incoming)
        .await?;
    Ok(())
}

/// Binds `addr` and serves the `tonic` gRPC `BigQuery` Storage API.
pub async fn serve_grpc(
    addr: SocketAddr,
    state: ServerState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    serve_grpc_with_listener(listener, state).await
}
