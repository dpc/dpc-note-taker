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

    /// Scroll to the inserted text on RPC commands (append/prepend)
    #[arg(long, env = "DPC_NT_SCROLL")]
    scroll: bool,

    /// Select the inserted text on RPC commands (append/prepend)
    #[arg(long, env = "DPC_NT_SELECT")]
    select: bool,

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
    #[serde(default)]
    focus: bool,
    #[serde(default)]
    scroll: bool,
    #[serde(default)]
    select: bool,
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

/// Cursor/selection placement requested by an RPC command
#[derive(Clone)]
struct CursorRequest {
    /// Character offset where the cursor should be placed
    offset: usize,
    /// If set, select from this character offset to `offset`
    select_from: Option<usize>,
    /// Whether to scroll the view to the cursor
    scroll: bool,
}

fn bind_rpc_socket(sock_path: &Path) -> anyhow::Result<UnixListener> {
    UnixListener::bind(sock_path)
        .with_context(|| format!("failed to bind socket at {}", sock_path.display()))
}

fn start_rpc_listener(
    listener: UnixListener,
    buffer: Arc<Mutex<String>>,
    request_focus: Arc<AtomicBool>,
    requested_cursor: Arc<Mutex<Option<CursorRequest>>>,
) {
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
            let (insert_start, insert_end) = match req.action.as_str() {
                "append" => {
                    let start = buf.chars().count();
                    buf.push_str(&req.content);
                    let end = buf.chars().count();
                    (start, end)
                }
                "prepend" => {
                    let end = req.content.chars().count();
                    *buf = format!("{}{buf}", req.content);
                    (0, end)
                }
                _ => {
                    let _ = stream.write_all(b"unknown action");
                    continue;
                }
            };
            if req.scroll || req.select {
                *requested_cursor.lock().unwrap() = Some(CursorRequest {
                    offset: insert_end,
                    select_from: if req.select {
                        Some(insert_start)
                    } else {
                        None
                    },
                    scroll: req.scroll,
                });
            }
            if req.focus {
                request_focus.store(true, Ordering::Relaxed);
            }
            let _ = stream.write_all(b"ok");
        }
    });
}

const EDITOR_ID: &str = "note_editor";

struct NoteApp {
    buffer: Arc<Mutex<String>>,
    request_focus: Arc<AtomicBool>,
    requested_cursor: Arc<Mutex<Option<CursorRequest>>>,
}

impl eframe::App for NoteApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint periodically to pick up RPC changes
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        if self.request_focus.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut buf = self.buffer.lock().unwrap();
            let editor_id = ui.make_persistent_id(EDITOR_ID);

            // Set cursor/selection before rendering so TextEdit picks it up.
            // Only consume the request once load_state succeeds (it returns
            // None until the TextEdit has been rendered at least once).
            let mut cursor_guard = self.requested_cursor.lock().unwrap();
            let cursor_applied = if let Some(ref cursor_req) = *cursor_guard
                && let Some(mut state) = egui::TextEdit::load_state(ctx, editor_id)
            {
                let primary = egui::text::CCursor::new(cursor_req.offset);
                let range = if let Some(from) = cursor_req.select_from {
                    let secondary = egui::text::CCursor::new(from);
                    egui::text::CCursorRange::two(secondary, primary)
                } else {
                    egui::text::CCursorRange::one(primary)
                };
                state.cursor.set_char_range(Some(range));
                state.store(ctx, editor_id);
                true
            } else {
                false
            };
            let pending_cursor = if cursor_applied {
                cursor_guard.take()
            } else {
                cursor_guard.clone()
            };
            drop(cursor_guard);

            if pending_cursor.is_some() {
                ctx.request_repaint();
            }

            let min_size = ui.available_size();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let output = egui::TextEdit::multiline(&mut *buf)
                        .id(editor_id)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .min_size(min_size)
                        .show(ui);

                    if let Some(ref cursor_req) = pending_cursor {
                        output.response.request_focus();
                        if cursor_req.scroll
                            && let Some(cursor_range) = &output.cursor_range
                        {
                            let cursor_rect = output
                                .galley
                                .pos_from_cursor(&cursor_range.primary)
                                .translate(output.galley_pos.to_vec2());
                            ui.scroll_to_rect(cursor_rect, Some(egui::Align::Center));
                        }
                    }
                });
        });
    }
}

/// Check if a session is already running, cleaning up stale sockets.
/// Returns true if an instance is alive.
fn instance_is_running(sock: &Path) -> bool {
    match UnixStream::connect(sock) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            let _ = std::fs::remove_file(sock);
            false
        }
        Err(_) => false,
    }
}

/// Start a new GUI instance (forks, parent returns immediately).
fn start_gui(session: &str, sock_path: &Path) -> anyhow::Result<()> {
    let buffer = Arc::new(Mutex::new(String::new()));
    let request_focus = Arc::new(AtomicBool::new(false));
    let requested_cursor: Arc<Mutex<Option<CursorRequest>>> = Arc::new(Mutex::new(None));

    // Bind the socket before forking so it's ready by the time the parent exits
    let listener = bind_rpc_socket(sock_path)?;

    // Fork: parent exits immediately, child continues with the GUI
    match unsafe { nix::unistd::fork() }.context("failed to fork")? {
        nix::unistd::ForkResult::Parent { .. } => return Ok(()),
        nix::unistd::ForkResult::Child => {
            let _ = nix::unistd::setsid();

            // Redirect inherited stdio to /dev/null so callers using
            // pipe-based output capture (e.g. Command::output()) don't
            // block waiting for our file descriptors to close.
            if let Ok(f) = std::fs::File::open("/dev/null") {
                use std::os::unix::io::AsRawFd;
                let fd = f.as_raw_fd();
                unsafe {
                    nix::libc::dup2(fd, 0);
                    nix::libc::dup2(fd, 1);
                    nix::libc::dup2(fd, 2);
                }
            }
        }
    }

    // Spawn the listener thread in the child process (threads don't survive fork)
    start_rpc_listener(
        listener,
        Arc::clone(&buffer),
        Arc::clone(&request_focus),
        Arc::clone(&requested_cursor),
    );

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

    let result = eframe::run_native(
        &format!("dpc-note-taker — {session}"),
        options,
        Box::new(move |_cc| {
            Ok(Box::new(NoteApp {
                buffer,
                request_focus,
                requested_cursor,
            }) as Box<dyn eframe::App>)
        }),
    );

    // This is the forked child — exit here so we don't fall back into main()
    let code = if result.is_ok() { 0 } else { 1 };
    std::process::exit(code);
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let sock_path = socket_path(&cli.session)?;

    let request = match cli.command {
        Some(Command::Prepend) => Some(RpcRequest {
            action: "prepend".into(),
            content: read_stdin()?,
            focus: cli.focus,
            scroll: cli.scroll,
            select: cli.select,
        }),
        Some(Command::Append) => Some(RpcRequest {
            action: "append".into(),
            content: read_stdin()?,
            focus: cli.focus,
            scroll: cli.scroll,
            select: cli.select,
        }),
        None => None,
    };

    if !instance_is_running(&sock_path) {
        start_gui(&cli.session, &sock_path)?;
    } else if request.is_none() {
        bail!(
            "session '{}' is already running. Use append/prepend to send text.",
            cli.session
        );
    }

    if let Some(request) = request {
        send_rpc(&sock_path, &request)?;
    }

    Ok(())
}
