// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use context::ServerContext;
use omicron_common::FileKv;
use slog::{debug, error, Drain};
use slog_dtrace::ProbeRegistration;
use slog_error_chain::SlogInlineError;
use std::error::Error;
use std::io;
use std::sync::Arc;

mod clickhouse_cli;
mod clickward;
mod config;
mod context;
mod http_entrypoints;

pub use clickhouse_cli::ClickhouseCli;
pub use clickward::Clickward;
pub use config::Config;

#[derive(Debug, thiserror::Error, SlogInlineError)]
pub enum StartError {
    #[error("failed to initialize logger")]
    InitializeLogger(#[source] io::Error),
    #[error("failed to register dtrace probes: {0}")]
    RegisterDtraceProbes(String),
    #[error("failed to initialize HTTP server")]
    InitializeHttpServer(#[source] Box<dyn Error + Send + Sync>),
}

pub type Server = dropshot::HttpServer<Arc<ServerContext>>;

/// Start the dropshot server for `clickhouse-admin-server` which
/// manages clickhouse replica servers.
pub async fn start_server_admin_server(
    clickward: Clickward,
    clickhouse_cli: ClickhouseCli,
    server_config: Config,
) -> Result<Server, StartError> {
    let (drain, registration) = slog_dtrace::with_drain(
        server_config
            .log
            .to_logger("clickhouse-admin-server")
            .map_err(StartError::InitializeLogger)?,
    );
    let log = slog::Logger::root(drain.fuse(), slog::o!(FileKv));
    match registration {
        ProbeRegistration::Success => {
            debug!(log, "registered DTrace probes");
        }
        ProbeRegistration::Failed(err) => {
            let err = StartError::RegisterDtraceProbes(err);
            error!(log, "failed to register DTrace probes"; &err);
            return Err(err);
        }
    }

    let context = ServerContext::new(
        clickward,
        clickhouse_cli
            .with_log(log.new(slog::o!("component" => "ClickhouseCli"))),
        log.new(slog::o!("component" => "ServerContext")),
    );
    let http_server_starter = dropshot::HttpServerStarter::new(
        &server_config.dropshot,
        http_entrypoints::clickhouse_admin_server_api(),
        Arc::new(context),
        &log.new(slog::o!("component" => "dropshot")),
    )
    .map_err(StartError::InitializeHttpServer)?;

    Ok(http_server_starter.start())
}

/// Start the dropshot server for `clickhouse-admin-server` which
/// manages clickhouse replica servers.
pub async fn start_keeper_admin_server(
    clickward: Clickward,
    clickhouse_cli: ClickhouseCli,
    server_config: Config,
) -> Result<Server, StartError> {
    let (drain, registration) = slog_dtrace::with_drain(
        server_config
            .log
            .to_logger("clickhouse-admin-keeper")
            .map_err(StartError::InitializeLogger)?,
    );
    let log = slog::Logger::root(drain.fuse(), slog::o!(FileKv));
    match registration {
        ProbeRegistration::Success => {
            debug!(log, "registered DTrace probes");
        }
        ProbeRegistration::Failed(err) => {
            let err = StartError::RegisterDtraceProbes(err);
            error!(log, "failed to register DTrace probes"; &err);
            return Err(err);
        }
    }

    let context = ServerContext::new(
        clickward,
        clickhouse_cli
            .with_log(log.new(slog::o!("component" => "ClickhouseCli"))),
        log.new(slog::o!("component" => "ServerContext")),
    );
    let http_server_starter = dropshot::HttpServerStarter::new(
        &server_config.dropshot,
        http_entrypoints::clickhouse_admin_keeper_api(),
        Arc::new(context),
        &log.new(slog::o!("component" => "dropshot")),
    )
    .map_err(StartError::InitializeHttpServer)?;

    Ok(http_server_starter.start())
}
