use chrono::Utc;
use std::{
    path::Path,
    process::ExitCode,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use sub2ops_server::{runtime::Runtime, settings::Settings};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const RESTART_MARKER: &str = "/tmp/sub2ops-supervision-fixture-restarted";

#[tokio::main]
async fn main() -> ExitCode {
    sub2ops_server::error::install_panic_redaction();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::from(1),
    }
}

async fn run() -> std::result::Result<(), ()> {
    let recovering = Path::new(RESTART_MARKER).exists();
    let settings = Settings::load(false).map_err(|_| ())?;
    let trigger = Arc::new(AtomicBool::new(false));
    let fired = Arc::new(AtomicBool::new(false));
    let clock_trigger = trigger.clone();
    let clock_fired = fired.clone();
    let clock = Arc::new(move || {
        if clock_trigger.load(Ordering::Acquire) && !clock_fired.swap(true, Ordering::AcqRel) {
            panic!("synthetic periodic clock failure");
        }
        Utc::now()
    });
    let runtime = Runtime::new_with_clock(settings, clock)
        .await
        .map_err(|_| ())?;
    let listener = TcpListener::bind(
        runtime
            .settings
            .bind
            .parse::<std::net::SocketAddr>()
            .map_err(|_| ())?,
    )
    .await
    .map_err(|_| ())?;
    let address = listener.local_addr().map_err(|_| ())?;
    let server_runtime = runtime.clone();
    let server = tokio::spawn(async move { server_runtime.serve(listener, shutdown()).await });
    wait_for_health(address).await?;

    if recovering {
        eprintln!("supervision_fixture_recovered");
    } else {
        // Docker only applies restart policies after a container has run for ten seconds.
        tokio::time::sleep(Duration::from_secs(11)).await;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(RESTART_MARKER)
            .map_err(|_| ())?;
        trigger.store(true, Ordering::Release);
    }

    match server.await.map_err(|_| ())? {
        Ok(()) => Ok(()),
        Err(_) if !recovering && fired.load(Ordering::Acquire) => {
            eprintln!("supervision_fixture_expected_failure");
            Err(())
        }
        Err(_) => Err(()),
    }
}

async fn wait_for_health(address: std::net::SocketAddr) -> std::result::Result<(), ()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Ok(mut stream)) =
            tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(address)).await
        {
            if stream
                .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .is_ok()
            {
                let mut response = Vec::new();
                if tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
                    .await
                    .is_ok_and(|result| result.is_ok())
                    && response.starts_with(b"HTTP/1.1 200 ")
                {
                    return Ok(());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
