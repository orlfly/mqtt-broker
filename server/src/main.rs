use std::sync::Arc;

use anyhow::Result;
use api::create_router;
use broker::MqttEngine;
use common::Config;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cfg = Config::load_default()?;
    info!(
        "Loaded config (mqtt {}, api {})",
        cfg.mqtt_bind_addr(),
        cfg.api_bind_addr()
    );

    info!("Starting MQTT Broker server");

    let engine = Arc::new(MqttEngine::new());
    let broker_state = engine.state();

    let jwt_auth = Arc::new(api::auth::JwtAuth::new(
        cfg.api.token.secret.clone(),
        cfg.api.token.expire_secs,
    ));
    let router = create_router(broker_state, jwt_auth);

    let api_addr = cfg.api_bind_addr();
    let mqtt_addr = cfg.mqtt_bind_addr();

    let api_listener = TcpListener::bind(&api_addr).await?;
    info!("REST API listening on {}", api_addr);

    let mqtt_engine = engine.clone();
    let mqtt_handle = tokio::spawn(async move {
        if let Err(e) = mqtt_engine.start(&mqtt_addr).await {
            error!("MQTT engine stopped with error: {}", e);
        }
    });

    let api_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(api_listener, router).await {
            error!("API server stopped with error: {}", e);
        }
    });

    tokio::select! {
        _ = mqtt_handle => {
            error!("MQTT task exited");
        }
        _ = api_handle => {
            error!("API task exited");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, shutting down");
        }
    }

    Ok(())
}
