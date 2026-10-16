use sub2ops_server::{
    error::{Error, Result},
    runtime::Runtime,
    settings::Settings,
};

#[tokio::main]
async fn main() {
    sub2ops_server::error::install_panic_redaction();
    if let Err(error) = run().await {
        // Never format nested SQL/HTTP/config errors or connection arguments.
        eprintln!(
            "Sub2Ops startup or shutdown failed ({})",
            error.status.as_u16()
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--version") {
        println!(
            "Sub2Ops {} rust {}",
            sub2ops_server::VERSION,
            sub2ops_server::BUILD_REVISION
        );
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help") {
        println!(
            "sub2ops-server [serve] [--read-only] [--check-state] [--host ADDRESS] [--port PORT]"
        );
        println!("sub2ops-server migrate-oauth --env-file PATH");
        println!("sub2ops-server migrate-desktop --env-file PATH [--keep-menu]");
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "migrate-oauth" | "migrate-desktop"))
    {
        let mut env_file = None;
        let mut keep_menu = false;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--env-file" if env_file.is_none() => {
                    index += 1;
                    env_file = Some(std::path::PathBuf::from(
                        args.get(index).ok_or_else(Error::invalid)?,
                    ));
                }
                "--keep-menu" if args[0] == "migrate-desktop" => keep_menu = true,
                _ => return Err(Error::invalid()),
            }
            index += 1;
        }
        let env_file = env_file.ok_or_else(Error::invalid)?;
        let env = std::env::vars().collect();
        let result = if args[0] == "migrate-oauth" {
            sub2ops_server::maintenance::migrate_oauth(&env, &env_file)?
        } else {
            sub2ops_server::maintenance::migrate_desktop(&env, &env_file, !keep_menu).await?
        };
        println!("{result}");
        return Ok(());
    }
    let check = args.iter().any(|arg| arg == "--check-state");
    let mut settings = Settings::load(check || args.iter().any(|arg| arg == "--read-only"))?;
    let mut host = None;
    let mut port = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "serve" | "--read-only" | "--check-state" => {}
            "--host" | "--port" => {
                let option = &args[index];
                index += 1;
                let value = args.get(index).ok_or_else(Error::invalid)?;
                if option == "--host" {
                    host = Some(value.clone());
                } else {
                    port = Some(value.parse::<u16>().map_err(|_| Error::invalid())?);
                }
            }
            _ => return Err(Error::invalid()),
        }
        index += 1;
    }
    if host.is_some() || port.is_some() {
        let current = settings
            .bind
            .parse::<std::net::SocketAddr>()
            .map_err(|_| Error::invalid())?;
        let host = host.unwrap_or_else(|| current.ip().to_string());
        let ip = host
            .parse::<std::net::IpAddr>()
            .map_err(|_| Error::invalid())?;
        settings.bind = std::net::SocketAddr::new(ip, port.unwrap_or(current.port())).to_string();
    }
    let address = settings.bind.clone();
    let runtime = Runtime::new(settings).await?;
    if check {
        println!("{}", runtime.preflight().await?);
        runtime.close().await;
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    let bound = listener.local_addr()?;
    let scope = if bound.ip().is_unspecified() {
        "all_interfaces"
    } else if bound.ip().is_loopback() {
        "loopback"
    } else {
        "configured_address_redacted"
    };
    eprintln!("Sub2Ops listening scope={scope} port={}", bound.port());
    runtime.serve(listener, shutdown()).await
}
async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=signal.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
