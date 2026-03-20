use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dpc-note-taker")]
struct Cli {
    /// Session name identifying the buffer instance
    #[arg(long, default_value = "default")]
    session: String,

    /// Focus the window on RPC commands (append/prepend)
    #[arg(long, env = "DPC_NT_FOCUS")]
    focus: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Read stdin and prepend it to the buffer
    Prepend,
    /// Read stdin and append it to the buffer
    Append,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RpcRequest {
    action: String,
    content: String,
}

fn socket_path(session: &str) -> anyhow::Result<PathBuf> {
    let runtime_dir = directories::BaseDirs::new()
        .context("failed to determine base directories")?
        .runtime_dir()
        .context("XDG_RUNTIME_DIR not set")?
        .join("dpc-note-taker");
    std::fs::create_dir_all(&runtime_dir)?;
    Ok(runtime_dir.join(format!("{session}.sock")))
}

fn read_stdin() -> anyhow::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn send_rpc(path: &Path, request: &RpcRequest) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(path).context("failed to connect to existing instance")?;
    let payload = serde_json::to_vec(request)?;
    stream.write_all(&payload)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if response == "ok" {
        Ok(())
    } else {
        bail!("instance returned error: {response}");
    }
}

fn try_connect_existing(sock: &Path, request: &RpcRequest) -> anyhow::Result<bool> {
    match UnixStream::connect(sock) {
        Ok(stream) => {
            // Connection succeeded — instance is alive
            drop(stream);
            send_rpc(sock, request)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            // Stale socket
            let _ = std::fs::remove_file(sock);
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn start_rpc_listener(
    sock_path: PathBuf,
    buffer: Arc<Mutex<String>>,
    request_focus: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("failed to bind socket at {}", sock_path.display()))?;

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut data = Vec::new();
            if stream.read_to_end(&mut data).is_err() {
                let _ = stream.write_all(b"error");
                continue;
            }
            let Ok(req) = serde_json::from_slice::<RpcRequest>(&data) else {
                let _ = stream.write_all(b"error");
                continue;
            };

            let mut buf = buffer.lock().unwrap();
            match req.action.as_str() {
                "append" => buf.push_str(&req.content),
                "prepend" => *buf = format!("{}{buf}", req.content),
                _ => {
                    let _ = stream.write_all(b"unknown action");
                    continue;
                }
            }
            if let Some(ref request_focus) = request_focus {
                request_focus.store(true, Ordering::Relaxed);
            }
            let _ = stream.write_all(b"ok");
        }
    });

    Ok(())
}

struct NoteApp {
    buffer: Arc<Mutex<String>>,
    request_focus: Option<Arc<AtomicBool>>,
}

impl eframe::App for NoteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint periodically to pick up RPC changes
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        if let Some(ref request_focus) = self.request_focus
            && request_focus.swap(false, Ordering::Relaxed)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut buf = self.buffer.lock().unwrap();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_sized(
                        ui.available_size(),
                        egui::TextEdit::multiline(&mut *buf)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
        });
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let sock_path = socket_path(&cli.session)?;

    match cli.command {
        Some(Command::Prepend) => {
            let content = read_stdin()?;
            let request = RpcRequest {
                action: "prepend".into(),
                content,
            };
            if try_connect_existing(&sock_path, &request)? {
                return Ok(());
            }
            // No instance running — start GUI with initial content
            run_gui(&cli.session, &sock_path, request.content, cli.focus)?;
        }
        Some(Command::Append) => {
            let content = read_stdin()?;
            let request = RpcRequest {
                action: "append".into(),
                content,
            };
            if try_connect_existing(&sock_path, &request)? {
                return Ok(());
            }
            run_gui(&cli.session, &sock_path, request.content, cli.focus)?;
        }
        None => {
            // No subcommand — just open GUI
            if sock_path.exists() {
                // Check if instance is alive
                match UnixStream::connect(&sock_path) {
                    Ok(_) => bail!(
                        "session '{}' is already running. Use append/prepend to send text.",
                        cli.session
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        let _ = std::fs::remove_file(&sock_path);
                    }
                    Err(_) => {}
                }
            }
            run_gui(&cli.session, &sock_path, String::new(), cli.focus)?;
        }
    }

    Ok(())
}

fn run_gui(
    session: &str,
    sock_path: &Path,
    initial_content: String,
    focus: bool,
) -> anyhow::Result<()> {
    let buffer = Arc::new(Mutex::new(initial_content));
    let request_focus = if focus {
        Some(Arc::new(AtomicBool::new(false)))
    } else {
        None
    };

    start_rpc_listener(
        sock_path.to_path_buf(),
        Arc::clone(&buffer),
        request_focus.clone(),
    )?;

    let sock_path = sock_path.to_path_buf();
    let _guard = scopeguard::guard((), |()| {
        let _ = std::fs::remove_file(&sock_path);
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("dpc-note-taker — {session}"))
            .with_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        &format!("dpc-note-taker — {session}"),
        options,
        Box::new(move |_cc| {
            Ok(Box::new(NoteApp {
                buffer,
                request_focus,
            }) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    Ok(())
}
