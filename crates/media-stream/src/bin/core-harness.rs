use media_stream::LocalRoomSession;
use shared_protocol::ClockEpochId;
use std::{path::PathBuf, sync::Arc};
use sync_engine::MonotonicClock;

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut file = None;
    let mut duration = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--file") if file.is_none() => file = args.next().map(PathBuf::from),
            Some("--duration-ms") if duration.is_none() => {
                duration = args
                    .next()
                    .and_then(|arg| arg.to_str().and_then(|value| value.parse::<u64>().ok()));
                if duration.is_none() {
                    return Err("invalid arguments".into());
                }
            }
            _ => return Err("invalid arguments".into()),
        }
    }
    let file = file.ok_or("missing selected file")?;
    let duration = duration.ok_or("missing duration")?;
    let session = LocalRoomSession::start(
        &file,
        duration,
        "host",
        Arc::new(MonotonicClock::new(ClockEpochId::new())),
    )
    .await?;
    println!("room-created");
    if let Err(error) = session.check_ready().await {
        session.shutdown().await?;
        return Err(error.into());
    }
    println!("range-server-ready");
    let signal = tokio::signal::ctrl_c().await;
    session.shutdown().await?;
    println!("room-closed");
    signal?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("core harness failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    if runtime.block_on(run()).is_err() {
        // Never format errors that may carry paths, input, or capabilities.
        eprintln!("core harness failed");
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
