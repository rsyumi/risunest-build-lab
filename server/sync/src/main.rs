#![cfg_attr(
    all(windows, feature = "windows-background"),
    windows_subsystem = "windows"
)]

use risunest_sync_server::{
    config::Config, connection::ConnectionOptions, http, management::Management, store::Store,
};
use std::{path::PathBuf, sync::Arc};

mod update;

const USAGE: &str = "risunest-sync-server <init|status|serve|maintain|backup|restore|restore-epoch|device add [--qr]|device revoke ID|connection configure|connection status|connection repost> --data-dir ABSOLUTE_PATH [--backup-dir ABSOLUTE_PATH] [--listen 127.0.0.1:14319] [--https-proxy]\nrisunest-sync-server update check\nconnection configure: choose --endpoint HTTP_OR_HTTPS_URL or --cloudflared ABSOLUTE_EXECUTABLE, optionally --registry REGISTRY_URL.\nUpdate check verifies the signed product catalog and reports the raw package for this OS and architecture without downloading or installing it.\nAdministration commands require the daemon to be stopped. Configured device add emits a private registration URI; --qr also displays its QR. Without connection configuration, manual credential JSON remains available. Backup and restore require a new destination directory. network configure --listen IP:PORT saves the listener for the next start; network status shows saved settings. The default listener is 127.0.0.1:14319. Listeners serve HTTP; an HTTPS proxy is optional.";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        if let Some(error) = error.downcast_ref::<risunest_sync_server::Error>() {
            let args = std::env::args().skip(1).collect::<Vec<_>>();
            if args.first().is_some_and(|command| command == "serve") {
                if let Some(path) = args
                    .windows(2)
                    .find(|pair| pair[0] == "--data-dir")
                    .map(|pair| PathBuf::from(&pair[1]))
                {
                    if let Ok(root) = risunest_sync_server::resolve_data_root(&path) {
                        if root.is_dir() {
                            use std::io::Write;
                            if let Ok(mut staged) = tempfile::Builder::new()
                                .prefix(".risunest-tmp-")
                                .tempfile_in(&root)
                            {
                                if staged.write_all(error.code.as_bytes()).is_ok()
                                    && staged.as_file().sync_all().is_ok()
                                {
                                    let _ = staged.persist(root.join("startup-error.txt"));
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!("{error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_none_or(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let command = args.next().unwrap();
    let subcommand = if command == "device"
        || command == "connection"
        || command == "update"
        || command == "network"
    {
        Some(args.next().ok_or(USAGE)?)
    } else {
        None
    };
    if command == "update" {
        if subcommand.as_deref() != Some("check") || args.next().is_some() {
            return Err(USAGE.into());
        }
        let status = update::check().await.map_err(|error| error.to_owned())?;
        println!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    let revoke = if subcommand.as_deref() == Some("revoke") {
        Some(args.next().ok_or(USAGE)?)
    } else {
        None
    };
    let mut data_dir = None;
    let mut backup_dir = None;
    let mut listen: Option<std::net::SocketAddr> = None;
    let mut https_proxy = false;
    let mut endpoint = None;
    let mut cloudflared = None;
    let mut registry_url = None;
    let mut qr = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" if data_dir.is_none() => {
                data_dir = Some(PathBuf::from(args.next().ok_or(USAGE)?))
            }
            "--listen" => listen = Some(args.next().ok_or(USAGE)?.parse()?),
            "--backup-dir" if backup_dir.is_none() => {
                backup_dir = Some(PathBuf::from(args.next().ok_or(USAGE)?))
            }
            "--https-proxy" => https_proxy = true,
            "--endpoint"
                if command == "connection"
                    && subcommand.as_deref() == Some("configure")
                    && endpoint.is_none() =>
            {
                endpoint = Some(args.next().ok_or(USAGE)?)
            }
            "--cloudflared"
                if command == "connection"
                    && subcommand.as_deref() == Some("configure")
                    && cloudflared.is_none() =>
            {
                cloudflared = Some(PathBuf::from(args.next().ok_or(USAGE)?))
            }
            "--registry"
                if command == "connection"
                    && subcommand.as_deref() == Some("configure")
                    && registry_url.is_none() =>
            {
                registry_url = Some(args.next().ok_or(USAGE)?)
            }
            "--qr" if command == "device" && subcommand.as_deref() == Some("add") && !qr => {
                qr = true
            }
            _ => return Err(USAGE.into()),
        }
    }
    let data_dir = data_dir.ok_or(USAGE)?;
    if !data_dir.is_absolute() {
        return Err("absolute-data-dir-required".into());
    }
    if command == "network" {
        use risunest_sync_server::config::NetworkSettings;
        match subcommand.as_deref() {
            Some("configure") => {
                let listen = listen.ok_or(USAGE)?;
                NetworkSettings {
                    schema: 1,
                    address: listen.ip(),
                    port: listen.port(),
                }
                .save(&data_dir)?;
            }
            Some("status") => (),
            _ => return Err(USAGE.into()),
        }
        println!(
            "{}",
            serde_json::to_string(&NetworkSettings::load(&data_dir)?)?
        );
        return Ok(());
    }
    let listen = match listen {
        Some(value) => value,
        None => risunest_sync_server::config::NetworkSettings::load(&data_dir)?.socket(),
    };
    let config = Config {
        data_dir,
        listen,
        https_proxy,
    };
    config.validate()?;
    if ![
        "init",
        "status",
        "serve",
        "maintain",
        "restore-epoch",
        "backup",
        "restore",
        "device",
        "connection",
    ]
    .contains(&command.as_str())
    {
        return Err(USAGE.into());
    }
    let store = if command == "restore" {
        Store::restore_backup(backup_dir.as_deref().ok_or(USAGE)?, &config.data_dir)?
    } else if command == "init" {
        Store::init(&config.data_dir)?
    } else {
        Store::open(&config.data_dir)?
    };
    match command.as_str() {
        "init" | "status" | "restore" => println!("{}", serde_json::to_string(&store.head()?)?),
        "backup" => println!(
            "{}",
            serde_json::to_string(&store.backup(backup_dir.as_deref().ok_or(USAGE)?)?)?
        ),
        "maintain" => println!("{}", serde_json::to_string(&store.maintain()?)?),
        "restore-epoch" => {
            store.rotate_restored_epoch()?;
            println!("{}", serde_json::to_string(&store.head()?)?);
        }
        "device" => match subcommand.as_deref() {
            Some("add") => {
                let connection = store.connection_status()?;
                // Offline issuance outlives this process; a Quick Tunnel changes on restart.
                if connection.mode == "managed" && !connection.directory_enabled {
                    return Err(risunest_sync_server::Error::new(
                        "managed-registration-needs-directory",
                        409,
                    )
                    .into());
                }
                if connection.mode == "unconfigured" && !qr {
                    println!("{}", serde_json::to_string(&store.add_device()?)?);
                } else {
                    let uri = store.issue_registration()?;
                    let qr_text = if qr {
                        Some(
                            qrcode::QrCode::with_error_correction_level(
                                uri.as_bytes(),
                                qrcode::EcLevel::M,
                            )
                            .map_err(|_| {
                                risunest_sync_server::Error::new("registration-qr-too-large", 413)
                            })?
                            .render::<qrcode::render::unicode::Dense1x2>()
                            .quiet_zone(true)
                            .build(),
                        )
                    } else {
                        None
                    };
                    println!("{uri}");
                    if let Some(text) = qr_text {
                        println!("{text}");
                    }
                }
            }
            Some("revoke") => store.revoke_device(&revoke.unwrap())?,
            _ => return Err(USAGE.into()),
        },
        "connection" => {
            match subcommand.as_deref() {
                Some("configure") => store.configure_connection(ConnectionOptions {
                    endpoint,
                    cloudflared,
                    registry_url,
                })?,
                Some("status") => (),
                Some("repost") => store.request_republication()?,
                _ => return Err(USAGE.into()),
            }
            println!("{}", serde_json::to_string(&store.connection_status()?)?);
        }
        "serve" => {
            let listener = match tokio::net::TcpListener::bind(config.listen).await {
                Ok(listener) => listener,
                Err(error) => {
                    let code = match error.kind() {
                        std::io::ErrorKind::AddrInUse => "listen-address-in-use",
                        std::io::ErrorKind::AddrNotAvailable => "listen-address-unavailable",
                        std::io::ErrorKind::PermissionDenied => "listen-permission-denied",
                        _ => "listener-start-failed",
                    };
                    let _ = std::fs::write(config.data_dir.join("startup-error.txt"), code);
                    return Err(format!("{code}: {}: {error}", config.listen).into());
                }
            };
            let origin = listener.local_addr()?;
            let store = Arc::new(store);
            // A service manager may send SIGTERM as soon as readiness is observable.
            let shutdown = shutdown();
            eprintln!(
                "sync listener ready: {} ({})",
                listener.local_addr()?,
                if config.https_proxy {
                    "HTTPS proxy origin"
                } else {
                    "local development"
                }
            );
            let workload = risunest_sync_server::workload::Workload::new();
            let management =
                Management::start_with_workload(store.clone(), origin, workload.clone()).await?;
            let mut stopped = management.shutdown_receiver();
            let stopping = store.clone();
            let (stream_stop, stream_stopped) = tokio::sync::watch::channel(false);
            let result = axum::serve(
                listener,
                http::router_with_shutdown(store, workload, stream_stopped),
            )
            .with_graceful_shutdown(async move {
                tokio::select! { _ = shutdown => (), _ = stopped.changed() => () }
                stream_stop.send_replace(true);
            })
            .await;
            management.close().await;
            // A clean stop folds the write-ahead log back, so the next start does
            // not rebuild its index over frames nothing needs.
            match stopping.checkpoint_wal() {
                Ok(checkpoint) if checkpoint.incomplete() => eprintln!(
                    "sync shutdown wal checkpoint incomplete: busy={} log={} checkpointed={}",
                    checkpoint.busy, checkpoint.log_frames, checkpoint.checkpointed_frames
                ),
                Ok(_) => (),
                Err(error) => eprintln!("sync shutdown wal checkpoint failed: {}", error.code),
            }
            result?;
        }
        _ => unreachable!(),
    }
    Ok(())
}
#[cfg(unix)]
fn shutdown() -> impl std::future::Future<Output = ()> {
    use tokio::signal::unix::{signal, SignalKind};
    let terminate = signal(SignalKind::terminate());
    async move {
        if let Ok(mut terminate) = terminate {
            tokio::select! { _=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{} }
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
#[cfg(not(unix))]
async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
