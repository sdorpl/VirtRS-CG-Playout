// VirtRS CG Playout sidecar: `virtrs-cg [config.json]` (or CONFIG=config.json).

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use virtrs_cg::config::Config;
use virtrs_cg::server::{MAX_BODY, Server};
use virtrs_cg::{error, http, info, log, sys, warn};

fn main() -> ExitCode {
    // Before any thread starts: SIGTERM and SIGINT go to the stop thread below.
    sys::block_stop_signals();
    log::set_debug(std::env::var("DEBUG").as_deref() == Ok("1"));

    let file = std::env::args_os().nth(1).or_else(|| std::env::var_os("CONFIG")).map(PathBuf::from);
    let config = match Config::load(file.as_deref()) {
        Ok((config, warnings)) => {
            for warning in warnings {
                warn!("{warning}");
            }
            config
        }
        Err(message) => {
            error!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let listener = match TcpListener::bind(config.listen_addr()) {
        Ok(listener) => listener,
        Err(e) => {
            error!("listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };

    let burn_in = if config.output_url.is_empty() { "off" } else { config.output_url.as_str() };
    info!("listening on {}; VizRS {}; burn-in {burn_in}", config.listen, config.vizrs.address);

    let server = Server::new(config);
    let stopping = Arc::clone(&server);
    std::thread::spawn(move || {
        sys::wait_for_stop();
        // Browsers and their profiles go with the sidecar.
        stopping.stop_captures();
        std::process::exit(0);
    });
    http::serve(listener, MAX_BODY, Arc::new(move |request, stream| server.handle(request, stream)));
    ExitCode::FAILURE
}
