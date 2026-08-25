use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use kma_audio_agent::{
    config::Config,
    connection,
    device::{Calibration, create_device},
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "kma-audio-agent", version)]
struct Cli {
    #[arg(long, default_value = "/etc/kma-audio-agent/config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Probe,
    Calibrate,
    Run,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("kma_audio_agent=info")),
        )
        .json()
        .with_current_span(false)
        .init();
    let cli = Cli::parse();
    let config = match Config::load(&cli.config).await {
        Ok(config) => config,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Config::default()
        }
        Err(error) => return Err(error),
    };
    match cli.command {
        Command::Probe => {
            let calibrated = Calibration::load(&config.calibration_path())
                .await?
                .is_some();
            let mut device = create_device(&config, String::new(), String::new())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&device.probe(&config, calibrated).await)?
            );
        }
        Command::Calibrate => {
            let mut device = create_device(&config, String::new(), String::new())?;
            let probe = device.probe(&config, false).await;
            anyhow::ensure!(probe.playback.ok, "FLOW 8 playback is unavailable");
            anyhow::ensure!(probe.capture.ok, "FLOW 8 capture is unavailable");
            let offset = device.calibrate(&config).await?;
            let calibration = Calibration {
                playback_to_capture_offset_frames: offset,
                sample_rate: 48_000,
                measured_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_secs()
                    .to_string(),
            };
            calibration.store(&config.calibration_path()).await?;
            println!("{}", serde_json::to_string_pretty(&calibration)?);
        }
        Command::Run => {
            tokio::select! {
                result = connection::run(config) => result?,
                _ = tokio::signal::ctrl_c() => tracing::info!("shutdown requested"),
            }
        }
    }
    Ok(())
}
