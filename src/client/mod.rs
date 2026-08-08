//! Thin client mode — connects to the server's client socket.
//!
//! The client:
//! - Connects to `herdr-client.sock`, sends Hello with terminal size and protocol version
//! - Sets up the real terminal (raw mode, mouse capture, keyboard enhancements)
//! - Receives Frame messages and blits them to the terminal (diff against last frame)
//! - Reads stdin events (keystrokes, mouse, paste) and sends them as ClientMessage::Input
//! - Detects terminal resize and sends ClientMessage::Resize
//! - Restores terminal on exit (normal or error)
//! - Handles ServerShutdown gracefully (clean exit, informative message to stderr)
//! - Handles server unreachable (clear error screen, not blank/hang)
//! - Forwards OSC 52 clipboard writes from server to its own stdout
//! - Displays sound/toast notifications forwarded from server

mod input;

use std::collections::HashSet;
use std::io::{self, BufRead, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use base64::Engine;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(unix)]
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
#[cfg(not(windows))]
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tracing::{debug, info, warn};
#[cfg(unix)]
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ipc::LocalStream;
use crate::protocol::render_ansi;
#[cfg(unix)]
use crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD;
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientKeybindings, ClientLaunchMode,
    ClientMessage, NotifyKind, RenderEncoding, ServerMessage, MAX_FRAME_SIZE,
    MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};
use crate::server::socket_paths::client_socket_path;

static RECEIVED_KITTY_GRAPHICS_IDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
static PINNED_CLIENT_KEYBINDINGS: OnceLock<ClientKeybindings> = OnceLock::new();

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

struct ClientLoopConfig {
    sound_config: crate::config::SoundConfig,
    mouse_scroll_lines: usize,
    redraw_on_focus_gained: bool,
    host_cursor: crate::config::HostCursorModeConfig,
    kitty_graphics_enabled: bool,
    mouse_capture_active: bool,
    #[cfg(unix)]
    palette: crate::app::state::Palette,
    #[cfg(unix)]
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
}

/// State tracking for the thin client.
struct ClientState {
    /// Stateful semantic-frame encoder used when the server sends FrameData.
    blit_encoder: render_ansi::BlitEncoder,
    /// Whether host mouse capture is currently active.
    mouse_capture_active: bool,
    /// The terminal size we reported to the server in our last Hello/Resize.
    reported_size: (u16, u16),
    /// Client-local sound playback config, refreshed on server request.
    sound_config: crate::config::SoundConfig,
    /// Whether this client may write Kitty graphics bytes to its host terminal.
    kitty_graphics_enabled: bool,
    /// Direct attach prefix escape state. None for full-app clients.
    attach_escape: Option<AttachEscapeState>,
    /// Rows scrolled for one direct-attach wheel notch.
    #[cfg(unix)]
    mouse_scroll_lines: usize,
    /// Local-client shortcut that sends a clipboard image to a remote Herdr session.
    #[cfg(unix)]
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    /// Whether outer focus gain should force a full host-terminal redraw.
    redraw_on_focus_gained: bool,
    /// Whether this client draws the cursor into frame cells instead of using the host cursor.
    draw_host_cursor: bool,
    /// Last authoritative semantic frame, before client-local overlays.
    #[cfg(unix)]
    last_semantic_frame: Option<crate::protocol::FrameData>,
    /// First-connect progress owned by this client, not either server runtime.
    #[cfg(unix)]
    federation_connecting: Option<FederationConnectingUi>,
    #[cfg(unix)]
    palette: crate::app::state::Palette,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FederationConnectingUi {
    member_label: String,
    tick: u64,
}

#[cfg(unix)]
impl FederationConnectingUi {
    fn for_plan(plan: FederationConnectionPlan, member_label: &str) -> Option<Self> {
        (plan == FederationConnectionPlan::New).then(|| Self {
            member_label: member_label.to_string(),
            tick: 0,
        })
    }

    fn message(&self) -> String {
        format!("Connecting to {}\u{2026}", self.member_label)
    }

    fn finish(active: &mut Option<Self>) -> bool {
        active.take().is_some()
    }
}

#[derive(Debug, Default)]
#[cfg(windows)]
struct AttachEscapeState;

#[derive(Debug, Default)]
#[cfg(unix)]
struct AttachEscapeState {
    pending_prefix: bool,
}

#[derive(Debug)]
#[cfg(unix)]
enum AttachInputAction {
    Forward(Vec<u8>),
    Scroll {
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    },
    Detach,
    None,
}

impl AttachEscapeState {
    #[cfg(unix)]
    fn filter_input(
        &mut self,
        data: Vec<u8>,
        viewport_rows: u16,
        mouse_scroll_lines: usize,
    ) -> AttachInputAction {
        const PREFIX: u8 = 0x02; // Ctrl+B

        let mut output = Vec::with_capacity(data.len());
        for byte in data {
            if self.pending_prefix {
                self.pending_prefix = false;
                match byte {
                    b'q' => return AttachInputAction::Detach,
                    PREFIX => output.push(PREFIX),
                    other => {
                        output.push(PREFIX);
                        output.push(other);
                    }
                }
                continue;
            }

            if byte == PREFIX {
                self.pending_prefix = true;
            } else {
                output.push(byte);
            }
        }

        if output.is_empty() {
            AttachInputAction::None
        } else if let Some(action) =
            attach_scroll_action(&output, viewport_rows, mouse_scroll_lines)
        {
            action
        } else {
            AttachInputAction::Forward(output)
        }
    }
}

#[cfg(unix)]
fn attach_scroll_action(
    data: &[u8],
    viewport_rows: u16,
    mouse_scroll_lines: usize,
) -> Option<AttachInputAction> {
    let mut events = crate::raw_input::parse_raw_input_bytes_sync(data);
    if events.len() != 1 {
        return None;
    }

    match events.pop()? {
        crate::raw_input::RawInputEvent::Mouse(mouse) => {
            let direction = match mouse.kind {
                MouseEventKind::ScrollUp => AttachScrollDirection::Up,
                MouseEventKind::ScrollDown => AttachScrollDirection::Down,
                _ => return Some(AttachInputAction::None),
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::Wheel,
                direction,
                lines: mouse_scroll_lines.max(1).min(u16::MAX as usize) as u16,
                column: Some(mouse.column),
                row: Some(mouse.row),
                modifiers: mouse.modifiers.bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
        {
            let direction = match key.code {
                KeyCode::PageUp => AttachScrollDirection::Up,
                KeyCode::PageDown => AttachScrollDirection::Down,
                _ => return None,
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::PageKey {
                    input: data.to_vec(),
                },
                direction,
                lines: viewport_rows.saturating_sub(1).max(1),
                column: None,
                row: None,
                modifiers: KeyModifiers::empty().bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && key.kind == KeyEventKind::Release
                && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) =>
        {
            Some(AttachInputAction::None)
        }
        _ => None,
    }
}

impl ClientState {
    fn request_full_redraw(&mut self) {
        self.blit_encoder = render_ansi::BlitEncoder::new();
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur during client operation.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the server's client socket.
    ConnectionFailed(io::Error),
    /// Server rejected our handshake.
    HandshakeRejected { version: u32, error: String },
    /// Server shut down.
    ServerShutdown { reason: Option<String> },
    /// Lost connection to the server.
    ConnectionLost(io::Error),
    /// Protocol error (framing, deserialization).
    Protocol(protocol::FramingError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(err) => {
                write!(f, "failed to connect to server: {err}")?;
                let path = client_socket_path();
                write!(
                    f,
                    "\nIs herdr server running? Start it with `herdr server`."
                )?;
                write!(f, "\nSocket path: {}", path.display())
            }
            ClientError::HandshakeRejected { version, error } => {
                write!(f, "server rejected handshake (version {version}): {error}")
            }
            ClientError::ServerShutdown { reason } => {
                match reason.as_deref() {
                    Some("detached") => {
                        if let Ok(reattach_command) =
                            std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                        {
                            write!(f, "detached from remote server")?;
                            write!(f, "\nRun `{reattach_command}` to reattach")?;
                        } else {
                            write!(f, "detached from server")?;
                            write!(
                                f,
                                "\nRun `{}` to reattach",
                                crate::session::local_attach_command()
                            )?;
                        }
                    }
                    _ => {
                        write!(f, "server shut down")?;
                        if let Some(reason) = reason {
                            write!(f, ": {reason}")?;
                        }
                    }
                }
                Ok(())
            }
            ClientError::ConnectionLost(err) => {
                if let Ok(reattach_command) = std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                {
                    write!(f, "lost connection to remote Herdr: {err}")?;
                    write!(f, "\nIf the remote server survived the SSH or network drop, its panes may still be running.")?;
                    write!(f, "\nRun `{reattach_command}` to reattach")
                } else {
                    write!(f, "lost connection to server: {err}")
                }
            }
            ClientError::Protocol(err) => {
                write!(f, "protocol error: {err}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(err) => Some(err),
            ClientError::ConnectionLost(err) => Some(err),
            ClientError::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<protocol::FramingError> for ClientError {
    fn from(err: protocol::FramingError) -> Self {
        match err {
            protocol::FramingError::UnexpectedEof => ClientError::ConnectionLost(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed connection",
            )),
            protocol::FramingError::Io(err) => ClientError::ConnectionLost(err),
            err => ClientError::Protocol(err),
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
fn setup_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. It enables mouse capture
/// so wheel events can drive the attached viewport or be forwarded to child
/// programs that requested mouse input.
fn setup_direct_attach_terminal() -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(false, true)
}

fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
) -> io::Result<TerminalGuard> {
    ratatui::init();
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let host_color_scheme_reports =
        should_enable_host_color_scheme_reports(enable_client_protocols);

    if enable_client_protocols {
        if mouse_capture {
            set_mouse_capture(true)?;
        } else {
            set_mouse_capture(false)?;
        }
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        if host_color_scheme_reports {
            write_host_color_scheme_report_mode(&mut io::stdout(), true)?;
        }
        push_keyboard_enhancement_flags()?;
    } else {
        if should_query_host_terminal_theme() {
            write_host_color_scheme_report_mode(&mut io::stdout(), false)?;
        }
        if mouse_capture {
            set_mouse_capture(true)?;
        } else {
            set_mouse_capture(false)?;
        }
    }

    #[cfg(windows)]
    let windows_virtual_terminal_input =
        if enable_client_protocols && windows_vti_input_backend_enabled() {
            enable_windows_virtual_terminal_input()
        } else {
            WindowsVirtualTerminalInputSetup::default()
        };

    #[cfg(windows)]
    if enable_client_protocols
        && windows_vti_input_backend_enabled()
        && windows_virtual_terminal_input.active
        && windows_win32_input_mode_enabled()
    {
        if let Err(err) = enable_windows_win32_input_mode(&mut io::stdout()) {
            if let Some(mode) = windows_virtual_terminal_input.restore_mode {
                restore_windows_input_mode_value(mode);
            }
            return Err(err);
        }
    }

    let modify_other_keys_mode = enable_client_protocols
        .then(crate::input::host_modify_other_keys_mode)
        .flatten();
    if let Some(mode) = modify_other_keys_mode {
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    Ok(TerminalGuard {
        reset_modify_other_keys: modify_other_keys_mode.is_some(),
        reset_host_color_scheme_reports: host_color_scheme_reports,
        #[cfg(windows)]
        restore_windows_input_mode: windows_virtual_terminal_input.restore_mode,
    })
}

fn should_enable_host_color_scheme_reports(enable_client_protocols: bool) -> bool {
    enable_client_protocols && should_query_host_terminal_theme()
}

/// Guard that restores the terminal when dropped.
struct TerminalGuard {
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    #[cfg(windows)]
    restore_windows_input_mode: Option<u32>,
}

fn write_host_color_scheme_report_mode(
    writer: &mut impl io::Write,
    enabled: bool,
) -> io::Result<()> {
    let sequence = if enabled {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE
    } else {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE
    };
    writer.write_all(sequence.as_bytes())?;
    writer.flush()
}

fn write_terminal_restore_postlude(
    writer: &mut impl io::Write,
    reset_host_color_scheme_reports: bool,
) -> io::Result<()> {
    if reset_host_color_scheme_reports {
        writer.write_all(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        )?;
    }
    // Restore a visible cursor and reset DECSCUSR back to the terminal default.
    writer.write_all(b"\x1b[?25h\x1b[0 q")?;
    writer.flush()
}

fn should_draw_host_cursor(mode: crate::config::HostCursorModeConfig) -> bool {
    match mode {
        crate::config::HostCursorModeConfig::Auto => {
            crate::platform::should_draw_host_cursor_by_default()
        }
        crate::config::HostCursorModeConfig::Native => false,
        crate::config::HostCursorModeConfig::Drawn => true,
    }
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsVirtualTerminalInputSetup {
    active: bool,
    restore_mode: Option<u32>,
}

#[cfg(windows)]
fn enable_windows_virtual_terminal_input() -> WindowsVirtualTerminalInputSetup {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
        STD_INPUT_HANDLE,
    };

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        tracing::warn!("failed to get Windows console input handle for VT input");
        return WindowsVirtualTerminalInputSetup::default();
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        tracing::warn!("failed to read Windows console input mode for VT input");
        return WindowsVirtualTerminalInputSetup::default();
    }

    let desired = windows_virtual_terminal_input_mode(mode);
    if desired == mode {
        return WindowsVirtualTerminalInputSetup {
            active: true,
            restore_mode: None,
        };
    }

    if unsafe { SetConsoleMode(handle, desired) } == 0 {
        tracing::warn!("failed to enable Windows virtual terminal input");
        return WindowsVirtualTerminalInputSetup::default();
    }

    let mut applied = 0;
    if unsafe { GetConsoleMode(handle, &mut applied) } == 0 {
        tracing::warn!("failed to verify Windows virtual terminal input mode");
        let _ = unsafe { SetConsoleMode(handle, mode) };
        return WindowsVirtualTerminalInputSetup::default();
    }
    if applied & ENABLE_VIRTUAL_TERMINAL_INPUT == 0 {
        tracing::warn!("Windows virtual terminal input bit did not stick");
        let _ = unsafe { SetConsoleMode(handle, mode) };
        return WindowsVirtualTerminalInputSetup::default();
    }

    WindowsVirtualTerminalInputSetup {
        active: true,
        restore_mode: Some(mode),
    }
}

#[cfg(windows)]
fn windows_vti_input_backend_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_BACKEND")
        .map(|backend| !backend.eq_ignore_ascii_case("crossterm"))
        .unwrap_or(true)
}

#[cfg(any(windows, test))]
fn windows_virtual_terminal_input_mode(mode: u32) -> u32 {
    mode | 0x0200
}

#[cfg(windows)]
fn restore_windows_input_mode_value(mode: u32) {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE};

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        tracing::warn!("failed to restore Windows console input mode");
    }
}

fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        match execute!(io::stdout(), DisableMouseCapture) {
            Ok(()) => Ok(()),
            #[cfg(windows)]
            Err(err) if err.to_string() == "Initial console modes not set" => Ok(()),
            Err(err) => Err(err),
        }
    }
}

fn restore_terminal_state(
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    #[cfg(windows)] restore_windows_input_mode: Option<u32>,
) {
    let _ = clear_received_kitty_graphics(&mut io::stdout());

    // Reset modifyOtherKeys if we enabled it.
    if reset_modify_other_keys {
        let _ = io::stdout().write_all(b"\x1b[>4;0m");
        let _ = io::stdout().flush();
    }

    let _ = pop_keyboard_enhancement_flags();

    let _ = execute!(
        io::stdout(),
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture
    );
    let _ = crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout());
    #[cfg(windows)]
    if let Some(mode) = restore_windows_input_mode {
        restore_windows_input_mode_value(mode);
    }

    ratatui::restore();
    let _ = write_terminal_restore_postlude(&mut io::stdout(), reset_host_color_scheme_reports);

    #[cfg(windows)]
    if windows_vti_input_backend_enabled() && windows_win32_input_mode_enabled() {
        let _ = disable_windows_win32_input_mode(&mut io::stdout());
    }
}

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
    )
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn windows_win32_input_mode_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_PROBE")
        .map(|probe| probe.eq_ignore_ascii_case("win32"))
        .unwrap_or(true)
}

#[cfg(windows)]
fn enable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001h")?;
    writer.flush()
}

#[cfg(windows)]
fn disable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001l")?;
    writer.flush()
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(
            self.reset_modify_other_keys,
            self.reset_host_color_scheme_reports,
            #[cfg(windows)]
            self.restore_windows_input_mode,
        );
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

fn requested_render_encoding() -> RenderEncoding {
    match std::env::var("HERDR_RENDER_ENCODING").ok().as_deref() {
        Some("terminal-ansi" | "terminal_ansi" | "ansi") => RenderEncoding::TerminalAnsi,
        _ => RenderEncoding::SemanticFrame,
    }
}

#[cfg(unix)]
fn is_remote_client_process() -> bool {
    std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR).is_ok()
}

/// Time to wait for the server's Welcome reply during the handshake.
///
/// A local client talks to an already-connected server, so 5s is plenty. The
/// remote bridge client (`herdr --remote`) sits behind a fresh per-attach ssh
/// connection whose cold-connect (TCP + key exchange + auth) happens inside this
/// window; on a high-latency link that easily exceeds 5s, so it gets a far
/// larger budget. See issue #753.
const LOCAL_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn handshake_read_timeout() -> Duration {
    #[cfg(unix)]
    if is_remote_client_process() {
        return REMOTE_HANDSHAKE_READ_TIMEOUT;
    }
    LOCAL_HANDSHAKE_READ_TIMEOUT
}

fn requested_keybindings() -> ClientKeybindings {
    PINNED_CLIENT_KEYBINDINGS
        .get_or_init(|| {
            // Pin the originating client's keymap once. Federation candidates
            // and live-handoff reconnects must not silently adopt a remote
            // server's conflicting bindings.
            crate::config::Config::load()
                .config
                .local_keybindings_profile_toml()
                .map(|keys_toml| ClientKeybindings::Local { keys_toml })
                .unwrap_or(ClientKeybindings::Server)
        })
        .clone()
}

#[cfg(windows)]
fn set_handshake_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    context: &'static str,
) -> Result<(), ClientError> {
    match stream.set_recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            debug!(err = %err, context, "client socket receive timeout unavailable");
            Ok(())
        }
        Err(err) => Err(ClientError::ConnectionFailed(err)),
    }
}

#[cfg(not(windows))]
fn set_handshake_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    _context: &'static str,
) -> Result<(), ClientError> {
    stream
        .set_recv_timeout(timeout)
        .map_err(ClientError::ConnectionFailed)
}

/// Performs the client→server handshake.
///
/// Sends Hello with the terminal size and protocol version, reads the Welcome
/// response. Returns Ok(()) on success, or an error if the server rejects us.
fn do_handshake(
    stream: &mut LocalStream,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    federation_candidate: bool,
) -> Result<RenderEncoding, ClientError> {
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // Send Hello.
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        keybindings: requested_keybindings(),
        launch_mode: if direct_attach_requested {
            ClientLaunchMode::TerminalAttach
        } else if federation_candidate {
            ClientLaunchMode::FederationCandidate
        } else {
            ClientLaunchMode::App
        },
    };
    protocol::write_message(stream, &hello)
        .map_err(|e| ClientError::ConnectionFailed(io::Error::other(e.to_string())))?;

    // Read Welcome.
    set_handshake_recv_timeout(
        stream,
        Some(if federation_candidate {
            REMOTE_HANDSHAKE_READ_TIMEOUT
        } else {
            handshake_read_timeout()
        }),
        "client handshake read timeout unavailable",
    )?;
    let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)?;
    set_handshake_recv_timeout(
        stream,
        None,
        "failed to clear client handshake read timeout",
    )?;

    match welcome {
        ServerMessage::Welcome {
            version,
            encoding,
            error,
        } => {
            if let Some(error) = error {
                return Err(ClientError::HandshakeRejected { version, error });
            }
            info!(version, ?encoding, "handshake succeeded");
            Ok(encoding)
        }
        _ => Err(ClientError::Protocol(protocol::FramingError::Io(
            io::Error::new(io::ErrorKind::InvalidData, "expected Welcome message"),
        ))),
    }
}

const LIVE_HANDOFF_RECONNECT_TIMEOUT: Duration = Duration::from_secs(30);

fn reconnect_after_live_handoff(
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    attach_request: Option<&(String, bool)>,
    kitty_graphics_enabled: bool,
) -> Result<(LocalStream, RenderEncoding), ClientError> {
    let socket_path = client_socket_path();
    let deadline = std::time::Instant::now() + LIVE_HANDOFF_RECONNECT_TIMEOUT;

    loop {
        let retry_error = match crate::ipc::connect_local_stream(&socket_path) {
            Ok(mut stream) => {
                let (cols, rows, cell_width_px, cell_height_px) =
                    current_terminal_geometry(kitty_graphics_enabled);
                match do_handshake(
                    &mut stream,
                    cols,
                    rows,
                    cell_width_px,
                    cell_height_px,
                    requested_encoding,
                    direct_attach_requested,
                    cfg!(unix),
                ) {
                    Ok(encoding) => {
                        if let Some((terminal_id, takeover)) = attach_request {
                            write_to_server(
                                &mut stream,
                                &ClientMessage::AttachTerminal {
                                    terminal_id: terminal_id.clone(),
                                    takeover: *takeover,
                                },
                            )
                            .map_err(ClientError::ConnectionLost)?;
                        }
                        return Ok((stream, encoding));
                    }
                    Err(err @ ClientError::HandshakeRejected { .. })
                    | Err(err @ ClientError::Protocol(_)) => return Err(err),
                    Err(err) => err,
                }
            }
            Err(err) => ClientError::ConnectionFailed(err),
        };

        if std::time::Instant::now() >= deadline {
            return Err(retry_error);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ---------------------------------------------------------------------------
// Client event loop
// ---------------------------------------------------------------------------

/// Internal events for the client event loop.
enum ClientLoopEvent {
    /// Raw input bytes from stdin.
    #[cfg(unix)]
    StdinInput(Vec<u8>),
    /// Structured input events from platforms without Unix-style stdin bytes.
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    /// Terminal resize detected.
    Resize(u16, u16, u32, u32),
    /// Server message received.
    ServerMessage {
        connection_id: u64,
        message: ServerMessage,
    },
    /// Server reader thread exited (connection lost).
    ServerDisconnected { connection_id: u64 },
    /// Timer tick.
    Timer,
}

#[cfg(unix)]
struct SuspendedClientConnection {
    member_id: String,
    server_id: Option<String>,
    session_id: Option<String>,
    stream: LocalStream,
    connection_id: u64,
    remote: Option<crate::remote::InPlaceRemoteConnection>,
    mouse_capture_active: bool,
    window_title: Option<String>,
    prefix_input_source_active: bool,
    is_remote_client: bool,
}

#[cfg(unix)]
struct ResumedClientConnection {
    member_id: String,
    server_id: Option<String>,
    session_id: Option<String>,
    stream: LocalStream,
    connection_id: u64,
    remote: Option<crate::remote::InPlaceRemoteConnection>,
    mouse_capture_active: bool,
    window_title: Option<String>,
    prefix_input_source_active: bool,
    is_remote_client: bool,
    reconnected_encoding: Option<RenderEncoding>,
}

#[cfg(unix)]
struct PendingFederationActivation {
    connection: ResumedClientConnection,
    endpoint_id: String,
    expected_resource: Option<crate::federation::FederatedResourceRef>,
    expected_runtime_identity: Option<(String, String)>,
    request_id: u64,
    retain_current: bool,
    restore_if_rejected: bool,
    deadline: Instant,
    buffered_messages: Vec<ServerMessage>,
}

#[cfg(unix)]
const FEDERATION_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const DIRECTORY_AUTHORITY_RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

fn spawn_server_reader(
    stream: &LocalStream,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    kitty_graphics_enabled: bool,
    connection_id: u64,
) -> Result<(), ClientError> {
    let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    let server_read_tx = event_tx.clone();
    let server_read_quit = should_quit.clone();
    let max_frame_size = if kitty_graphics_enabled {
        MAX_GRAPHICS_FRAME_SIZE
    } else {
        MAX_FRAME_SIZE
    };
    std::thread::spawn(move || {
        server_reader_thread(
            read_stream,
            server_read_tx,
            &server_read_quit,
            max_frame_size,
            connection_id,
        );
    });
    Ok(())
}

#[cfg(unix)]
fn handshake_current_terminal(
    mut stream: LocalStream,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    kitty_graphics_enabled: bool,
    federation_candidate: bool,
) -> Result<(LocalStream, RenderEncoding), ClientError> {
    let (cols, rows, cell_width_px, cell_height_px) =
        current_terminal_geometry(kitty_graphics_enabled);
    let encoding = do_handshake(
        &mut stream,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        direct_attach_requested,
        federation_candidate,
    )?;
    Ok((stream, encoding))
}

#[cfg(unix)]
fn reconnect_suspended_connection(
    remote: Option<&crate::remote::InPlaceRemoteConnection>,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    kitty_graphics_enabled: bool,
) -> Result<(LocalStream, RenderEncoding), ClientError> {
    let stream = if let Some(remote) = remote {
        remote.connect().map_err(ClientError::ConnectionFailed)?
    } else {
        crate::ipc::connect_local_stream(&client_socket_path())
            .map_err(ClientError::ConnectionFailed)?
    };
    handshake_current_terminal(
        stream,
        requested_encoding,
        direct_attach_requested,
        kitty_graphics_enabled,
        true,
    )
}

#[cfg(unix)]
fn reconnect_remote_after_live_handoff(
    remote: &crate::remote::InPlaceRemoteConnection,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    kitty_graphics_enabled: bool,
) -> Result<(LocalStream, RenderEncoding), ClientError> {
    let deadline = Instant::now() + LIVE_HANDOFF_RECONNECT_TIMEOUT;
    loop {
        let attempt = remote
            .connect()
            .map_err(ClientError::ConnectionFailed)
            .and_then(|stream| {
                handshake_current_terminal(
                    stream,
                    requested_encoding,
                    direct_attach_requested,
                    kitty_graphics_enabled,
                    true,
                )
            });
        match attempt {
            Ok(connection) => return Ok(connection),
            Err(err) if Instant::now() < deadline => {
                debug!(%err, "remote replacement is not ready; retrying in-place handshake");
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn resume_suspended_connection(
    suspended: SuspendedClientConnection,
    reconnect: bool,
    next_connection_id: u64,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    kitty_graphics_enabled: bool,
) -> Result<ResumedClientConnection, ClientError> {
    let SuspendedClientConnection {
        member_id,
        server_id,
        session_id,
        stream,
        connection_id,
        remote,
        mouse_capture_active,
        window_title,
        prefix_input_source_active,
        is_remote_client,
    } = suspended;
    if reconnect {
        let (stream, encoding) = reconnect_suspended_connection(
            remote.as_ref(),
            requested_encoding,
            direct_attach_requested,
            kitty_graphics_enabled,
        )?;
        Ok(ResumedClientConnection {
            member_id,
            server_id,
            session_id,
            stream,
            connection_id: next_connection_id,
            remote,
            mouse_capture_active,
            window_title,
            prefix_input_source_active,
            is_remote_client,
            reconnected_encoding: Some(encoding),
        })
    } else {
        Ok(ResumedClientConnection {
            member_id,
            server_id,
            session_id,
            stream,
            connection_id,
            remote,
            mouse_capture_active,
            window_title,
            prefix_input_source_active,
            is_remote_client,
            reconnected_encoding: None,
        })
    }
}

/// Runs the thin client: connects to the server, performs the handshake,
/// and enters the main event loop.
///
/// This is the entry point called from `main.rs` when running in client mode.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(
        requested_render_encoding(),
        None,
        None,
        "connecting to server",
    )
}

/// Runs a direct terminal attach client.
#[cfg(unix)]
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        RenderEncoding::TerminalAnsi,
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
    )
}

/// Direct terminal attach is Unix raw-byte input only until Windows gets a semantic attach path.
#[cfg(windows)]
pub fn run_terminal_attach(_terminal_id: String, _takeover: bool) -> io::Result<()> {
    debug_assert!(!crate::platform::capabilities().direct_terminal_attach);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "direct terminal attach is not supported on Windows yet",
    ))
}

/// Runs a read-only terminal session observer and prints one JSON envelope per frame.
pub fn run_terminal_session_observe(target: String, cols: u16, rows: u16) -> io::Result<()> {
    let mut stream =
        connect_terminal_session_stream(target.clone(), cols, rows, "observing terminal session")?;
    write_to_server(&mut stream, &ClientMessage::ObserveTerminal { target })?;
    write_terminal_session_output(stream)
}

/// Runs a writable terminal session controller.
pub fn run_terminal_session_control(
    target: String,
    takeover: bool,
    cols: u16,
    rows: u16,
) -> io::Result<()> {
    let mut stream = connect_terminal_session_stream(
        target.clone(),
        cols,
        rows,
        "controlling terminal session",
    )?;
    write_to_server(
        &mut stream,
        &ClientMessage::ControlTerminal { target, takeover },
    )?;

    let mut write_stream = stream.try_clone()?;
    let _input_thread = std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            match terminal_control_command_from_json(&line) {
                Ok(message) => {
                    let release = matches!(message, ClientMessage::Detach);
                    if write_to_server(&mut write_stream, &message).is_err() {
                        return;
                    }
                    if release {
                        return;
                    }
                }
                Err(err) => eprintln!("herdr: terminal session control input ignored: {err}"),
            }
        }
        let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
    });

    write_terminal_session_output(stream)
}

fn connect_terminal_session_stream(
    target: String,
    cols: u16,
    rows: u16,
    log_message: &'static str,
) -> io::Result<LocalStream> {
    init_logging();

    let socket_path = client_socket_path();
    crate::logging::startup("client");
    info!(path = %socket_path.display(), target = %target, cols, rows, "{log_message}");

    let mut stream = match crate::ipc::connect_local_stream(&socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("herdr: {}", ClientError::ConnectionFailed(err));
            std::process::exit(1);
        }
    };

    match do_handshake(
        &mut stream,
        cols,
        rows,
        0,
        0,
        RenderEncoding::TerminalAnsi,
        true,
        false,
    ) {
        Ok(RenderEncoding::TerminalAnsi) => {}
        Ok(encoding) => {
            eprintln!(
                "herdr: terminal session observe negotiated unsupported encoding {encoding:?}"
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
    }

    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn write_terminal_session_output(mut stream: LocalStream) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    loop {
        match protocol::read_message(&mut stream, MAX_GRAPHICS_FRAME_SIZE) {
            Ok(ServerMessage::Terminal(frame)) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&frame.bytes);
                let line = serde_json::json!({
                    "type": "terminal.frame",
                    "seq": frame.seq,
                    "encoding": "ansi",
                    "width": frame.width,
                    "height": frame.height,
                    "full": frame.full,
                    "bytes": encoded,
                });
                serde_json::to_writer(&mut stdout, &line)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
            }
            Ok(ServerMessage::ServerShutdown { reason }) => {
                let line = serde_json::json!({
                    "type": "terminal.closed",
                    "reason": reason,
                });
                serde_json::to_writer(&mut stdout, &line)?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
                return Ok(());
            }
            Ok(ServerMessage::Graphics { .. }) => {}
            Ok(_) => {}
            Err(protocol::FramingError::UnexpectedEof) => return Ok(()),
            Err(err) => return Err(io::Error::other(err.to_string())),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum TerminalControlCommand {
    #[serde(rename = "terminal.input")]
    Input {
        text: Option<String>,
        bytes: Option<String>,
    },
    #[serde(rename = "terminal.resize")]
    Resize {
        cols: u16,
        rows: u16,
        #[serde(default)]
        cell_width_px: u32,
        #[serde(default)]
        cell_height_px: u32,
    },
    #[serde(rename = "terminal.scroll")]
    Scroll {
        direction: TerminalControlScrollDirection,
        lines: u16,
        #[serde(default)]
        source: TerminalControlScrollSource,
        #[serde(default)]
        column: Option<u16>,
        #[serde(default)]
        row: Option<u16>,
        #[serde(default)]
        modifiers: u8,
    },
    #[serde(rename = "terminal.release")]
    Release {},
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalControlScrollDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalControlScrollSource {
    #[default]
    Wheel,
    PageKey,
}

fn terminal_control_command_from_json(raw: &str) -> Result<ClientMessage, String> {
    let command = serde_json::from_str::<TerminalControlCommand>(raw)
        .map_err(|err| format!("invalid json command: {err}"))?;
    match command {
        TerminalControlCommand::Input { text, bytes } => {
            let data = match (text, bytes) {
                (Some(_), Some(_)) => {
                    return Err("terminal.input accepts text or bytes, not both".into())
                }
                (Some(text), None) => text.into_bytes(),
                (None, Some(bytes)) => base64::engine::general_purpose::STANDARD
                    .decode(bytes)
                    .map_err(|err| format!("invalid terminal.input bytes: {err}"))?,
                (None, None) => Vec::new(),
            };
            Ok(ClientMessage::Input { data })
        }
        TerminalControlCommand::Resize {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        } => {
            if cols == 0 || rows == 0 {
                return Err("terminal.resize cols and rows must be greater than 0".into());
            }
            Ok(ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            })
        }
        TerminalControlCommand::Scroll {
            direction,
            lines,
            source,
            column,
            row,
            modifiers,
        } => {
            if lines == 0 {
                return Err("terminal.scroll lines must be greater than 0".into());
            }
            let direction = match direction {
                TerminalControlScrollDirection::Up => AttachScrollDirection::Up,
                TerminalControlScrollDirection::Down => AttachScrollDirection::Down,
            };
            let source = match source {
                TerminalControlScrollSource::Wheel => AttachScrollSource::Wheel,
                TerminalControlScrollSource::PageKey => AttachScrollSource::PageKey {
                    input: match direction {
                        AttachScrollDirection::Up => b"\x1b[5~".to_vec(),
                        AttachScrollDirection::Down => b"\x1b[6~".to_vec(),
                    },
                },
            };
            Ok(ClientMessage::AttachScroll {
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            })
        }
        TerminalControlCommand::Release {} => Ok(ClientMessage::Detach),
    }
}

fn run_client_with_mode(
    requested_encoding: RenderEncoding,
    attach_request: Option<(String, bool)>,
    attach_escape: Option<AttachEscapeState>,
    log_message: &'static str,
) -> io::Result<()> {
    init_logging();

    let loaded_config = crate::config::Config::load();
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let mouse_capture = loaded_config.config.ui.mouse_capture;
    let mouse_scroll_lines = loaded_config.config.ui.mouse_scroll_lines();
    let redraw_on_focus_gained = loaded_config.config.ui.redraw_on_focus_gained;
    let host_cursor = loaded_config.config.ui.host_cursor;
    let direct_attach_requested = attach_request.is_some();
    #[cfg(unix)]
    let remote_image_paste_key = client_remote_image_paste_key(&loaded_config.config);
    let kitty_graphics_enabled =
        loaded_config.config.experimental.kitty_graphics && !direct_attach_requested;
    let loop_config = ClientLoopConfig {
        sound_config: loaded_config.config.ui.sound,
        mouse_scroll_lines,
        redraw_on_focus_gained,
        host_cursor,
        kitty_graphics_enabled,
        mouse_capture_active: mouse_capture,
        #[cfg(unix)]
        palette: client_palette(&loaded_config.config.theme),
        #[cfg(unix)]
        remote_image_paste_key,
    };

    let socket_path = client_socket_path();
    crate::logging::startup("client");
    info!(path = %socket_path.display(), "{log_message}");

    // Try to connect to the server.
    let mut stream = match crate::ipc::connect_local_stream(&socket_path) {
        Ok(s) => s,
        Err(err) => {
            // Server unreachable — show clear error and exit.
            let client_err = ClientError::ConnectionFailed(err);
            eprintln!("herdr: {client_err}");
            std::process::exit(1);
        }
    };

    // Get the terminal geometry before handshake (before raw mode).
    let (cols, rows, cell_width_px, cell_height_px) =
        current_terminal_geometry(kitty_graphics_enabled);

    // Perform handshake while the stream is still in blocking mode.
    let negotiated_encoding = match do_handshake(
        &mut stream,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        direct_attach_requested,
        false,
    ) {
        Ok(encoding) => encoding,
        Err(err) => {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
    };

    if let Some((terminal_id, takeover)) = &attach_request {
        let attach = ClientMessage::AttachTerminal {
            terminal_id: terminal_id.clone(),
            takeover: *takeover,
        };
        if let Err(err) = write_to_server(&mut stream, &attach) {
            eprintln!("herdr: failed to request terminal attach: {err}");
            std::process::exit(1);
        }
    }

    // Now set up the terminal. This must happen AFTER the handshake succeeds,
    // so we don't leave the terminal in raw mode if the server rejects us.
    let direct_attach = attach_escape.is_some();
    let terminal_guard = if direct_attach {
        setup_direct_attach_terminal()
    } else {
        setup_terminal(mouse_capture)
    }
    .map_err(|err| {
        eprintln!("herdr: failed to set up terminal: {err}");
        err
    })?;

    // Install a panic hook to restore the terminal on panic (same as monolithic).
    let panic_resets_modify_other_keys = terminal_guard.reset_modify_other_keys;
    let panic_resets_host_color_scheme_reports = terminal_guard.reset_host_color_scheme_reports;
    #[cfg(windows)]
    let panic_restore_windows_input_mode = terminal_guard.restore_windows_input_mode;
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_state(
            panic_resets_modify_other_keys,
            panic_resets_host_color_scheme_reports,
            #[cfg(windows)]
            panic_restore_windows_input_mode,
        );
        original_hook(info);
    }));

    // Create the tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let should_quit = Arc::new(AtomicBool::new(false));

    // Install Ctrl+C handler.
    let quit_flag = should_quit.clone();
    let _ = ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Release);
    });

    let result = rt.block_on(async {
        run_client_loop(
            stream,
            cols,
            rows,
            should_quit,
            loop_config,
            requested_encoding,
            negotiated_encoding,
            attach_request,
            attach_escape,
            loaded_config.config.federation.member_id.clone(),
        )
        .await
    });

    // Restore the terminal before printing any final status message.
    drop(terminal_guard);

    if let Err(err) = result {
        eprintln!("herdr: {err}");
        rt.shutdown_timeout(Duration::from_millis(100));
        crate::logging::shutdown("client");

        if matches!(
            err,
            ClientError::ServerShutdown {
                reason: Some(reason)
            } if reason == "detached"
        ) {
            return Ok(());
        }

        std::process::exit(1);
    }

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("client");
    Ok(())
}

#[cfg(unix)]
fn client_palette(theme: &crate::config::ThemeConfig) -> crate::app::state::Palette {
    let name = theme.name.as_deref().unwrap_or("catppuccin");
    let mut palette = crate::app::state::Palette::from_name(name)
        .unwrap_or_else(crate::app::state::Palette::catppuccin);
    if let Some(custom) = &theme.custom {
        palette = palette.with_overrides(custom);
    }
    palette
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FederationConnectionPlan {
    Current,
    Suspended(usize),
    New,
}

#[cfg(unix)]
fn federation_connection_plan<'a>(
    active_member_id: &str,
    mut suspended_member_ids: impl Iterator<Item = &'a str>,
    destination_member_id: &str,
) -> FederationConnectionPlan {
    if active_member_id == destination_member_id {
        return FederationConnectionPlan::Current;
    }
    suspended_member_ids
        .position(|member_id| member_id == destination_member_id)
        .map_or(
            FederationConnectionPlan::New,
            FederationConnectionPlan::Suspended,
        )
}

#[cfg(unix)]
fn federation_connecting_frame(
    base: &crate::protocol::FrameData,
    connecting: &FederationConnectingUi,
    palette: &crate::app::state::Palette,
) -> crate::protocol::FrameData {
    use ratatui::style::Modifier;

    let mut frame = base.clone();
    frame.cursor = frame.cursor.map(|mut cursor| {
        cursor.visible = false;
        cursor
    });
    frame.graphics.clear();
    if frame.width < 12 || frame.height < 3 {
        return frame;
    }

    let max_content_width = frame.width.saturating_sub(6) as usize;
    let message = crate::ui::text::truncate_end(&connecting.message(), max_content_width);
    let message_width = UnicodeWidthStr::width(message.as_str()) as u16;
    let popup_width = message_width.saturating_add(4).min(frame.width);
    let x = frame.width.saturating_sub(popup_width) / 2;
    let y = frame.height.saturating_sub(3) / 2;
    let bg = crate::protocol::color_to_u32(palette.panel_bg);
    let border_fg = crate::protocol::color_to_u32(palette.accent);
    let text_fg = crate::protocol::color_to_u32(palette.text);
    let bold = crate::protocol::modifier_to_u16(Modifier::BOLD);

    let mut put = |column: u16, row: u16, symbol: &str, fg: u32, modifier: u16, skip: bool| {
        let index = (row as usize)
            .saturating_mul(frame.width as usize)
            .saturating_add(column as usize);
        if let Some(cell) = frame.cells.get_mut(index) {
            cell.symbol = symbol.to_string();
            cell.fg = fg;
            cell.bg = bg;
            cell.modifier = modifier;
            cell.skip = skip;
            cell.hyperlink = None;
        }
    };

    for row in y..y.saturating_add(3) {
        for column in x..x.saturating_add(popup_width) {
            put(column, row, " ", text_fg, 0, false);
        }
    }
    put(x, y, "\u{256d}", border_fg, 0, false);
    put(x + popup_width - 1, y, "\u{256e}", border_fg, 0, false);
    put(x, y + 1, "\u{2502}", border_fg, 0, false);
    put(x + popup_width - 1, y + 1, "\u{2502}", border_fg, 0, false);
    put(x, y + 2, "\u{2570}", border_fg, 0, false);
    put(x + popup_width - 1, y + 2, "\u{256f}", border_fg, 0, false);
    for column in x + 1..x + popup_width - 1 {
        put(column, y, "\u{2500}", border_fg, 0, false);
        put(column, y + 2, "\u{2500}", border_fg, 0, false);
    }

    let spinner = ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"][(connecting.tick as usize) % 4];
    put(x + 1, y + 1, spinner, border_fg, bold, false);
    let mut column = x + 3;
    for ch in message.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if width == 0 || column.saturating_add(width) > x + popup_width - 1 {
            continue;
        }
        let mut encoded = [0_u8; 4];
        put(
            column,
            y + 1,
            ch.encode_utf8(&mut encoded),
            text_fg,
            bold,
            false,
        );
        if width == 2 {
            let trailing = column + 1;
            put(trailing, y + 1, " ", text_fg, bold, true);
        }
        column = column.saturating_add(width);
    }
    frame
}

#[cfg(unix)]
fn display_semantic_frame(
    state: &mut ClientState,
    frame: crate::protocol::FrameData,
    remember_authoritative: bool,
) {
    if remember_authoritative {
        state.last_semantic_frame = Some(frame.clone());
    }
    let frame = state
        .federation_connecting
        .as_ref()
        .map_or(frame.clone(), |connecting| {
            federation_connecting_frame(&frame, connecting, &state.palette)
        });
    let encoded = if state.draw_host_cursor {
        state
            .blit_encoder
            .encode_with_suppressed_visible_cursor(&frame, false)
    } else {
        state.blit_encoder.encode(&frame, false)
    };
    let mut stdout = io::stdout();
    let graphics = if state.kitty_graphics_enabled {
        frame.graphics.as_slice()
    } else {
        &[]
    };
    let _ = write_encoded_frame_with_graphics(&mut stdout, &encoded.bytes, graphics);
    let _ = stdout.flush();
    state.blit_encoder.commit(frame, encoded);
}

#[cfg(unix)]
fn redraw_connecting_ui(state: &mut ClientState) {
    let Some(frame) = state.last_semantic_frame.clone() else {
        return;
    };
    display_semantic_frame(state, frame, false);
}

#[cfg(unix)]
fn clear_connecting_ui(state: &mut ClientState, restore_source: bool) {
    if !FederationConnectingUi::finish(&mut state.federation_connecting) {
        return;
    }
    if restore_source {
        if let Some(frame) = state.last_semantic_frame.clone() {
            display_semantic_frame(state, frame, false);
        }
    }
}

#[cfg(unix)]
fn federation_activation_identity_matches(
    expected_member_id: &str,
    expected_resource: Option<&crate::federation::FederatedResourceRef>,
    expected_runtime_identity: Option<&(String, String)>,
    member_id: &str,
    server_id: &str,
    session_id: &str,
) -> bool {
    member_id == expected_member_id
        && expected_resource.is_none_or(|resource| {
            resource.server_id == server_id && resource.session_id == session_id
        })
        && expected_runtime_identity.is_none_or(|(expected_server, expected_session)| {
            expected_server == server_id && expected_session == session_id
        })
}

#[cfg(unix)]
fn federated_endpoint_runtime_identity(
    state: &crate::federation::EndpointState,
) -> Option<(String, String)> {
    state.snapshot.as_ref().map(|snapshot| {
        (
            snapshot.identity.server_id.clone(),
            snapshot.identity.session_id.clone(),
        )
    })
}

#[cfg(unix)]
fn directory_authority_identity_matches(
    connection_member_id: &str,
    connection_server_id: Option<&str>,
    connection_session_id: Option<&str>,
    authority_member_id: &str,
    member_id: &str,
    server_id: &str,
    session_id: &str,
) -> bool {
    connection_member_id == authority_member_id
        && connection_member_id == member_id
        && connection_server_id == Some(server_id)
        && connection_session_id == Some(session_id)
}

#[cfg(unix)]
fn merge_federation_directory(
    directory: &mut Vec<crate::federation::EndpointState>,
    incoming: Vec<crate::federation::EndpointState>,
) {
    let previous = std::mem::take(directory);
    for mut state in incoming {
        if let Some(existing) = previous
            .iter()
            .find(|existing| existing.endpoint.id == state.endpoint.id)
        {
            // A temporary disconnect must not erase the last useful global
            // directory snapshot that the client already has.
            if state.snapshot.is_none() {
                state.snapshot = existing.snapshot.clone();
                state.cursor = state.cursor.or(existing.cursor);
            }
        }
        directory.push(state);
    }
    directory.sort_by(|left, right| left.endpoint.id.cmp(&right.endpoint.id));
}

#[cfg(unix)]
fn activate_federation_connection(
    stream: &mut LocalStream,
    request_id: u64,
    expected_member_id: &str,
    resource: Option<crate::federation::FederatedResourceRef>,
    directory: &[crate::federation::EndpointState],
    presentation: Option<crate::protocol::FederationPresentation>,
) -> io::Result<()> {
    write_to_server(
        stream,
        &ClientMessage::FederationActivate {
            request_id,
            expected_member_id: expected_member_id.to_string(),
            resource,
            directory: directory.to_vec(),
            presentation,
        },
    )
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn begin_suspended_fallback(
    suspended_connections: &mut Vec<SuspendedClientConnection>,
    disconnected_connections: &mut HashSet<u64>,
    next_connection_id: &mut u64,
    next_activation_request_id: &mut u64,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    kitty_graphics_enabled: bool,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    directory: &[crate::federation::EndpointState],
) -> Result<Option<PendingFederationActivation>, ClientError> {
    while let Some(suspended) = suspended_connections.pop() {
        let reconnect = disconnected_connections.remove(&suspended.connection_id);
        let resumed = match resume_suspended_connection(
            suspended,
            reconnect,
            *next_connection_id,
            requested_encoding,
            direct_attach_requested,
            kitty_graphics_enabled,
        ) {
            Ok(resumed) => resumed,
            Err(err) => {
                warn!(%err, "skipping unhealthy suspended Herdr connection");
                continue;
            }
        };
        if resumed.reconnected_encoding.is_some() {
            *next_connection_id = next_connection_id.wrapping_add(1);
            spawn_server_reader(
                &resumed.stream,
                event_tx,
                should_quit,
                kitty_graphics_enabled,
                resumed.connection_id,
            )?;
        }
        let request_id = *next_activation_request_id;
        *next_activation_request_id = next_activation_request_id.wrapping_add(1);
        let mut resumed = resumed;
        if let Err(err) = activate_federation_connection(
            &mut resumed.stream,
            request_id,
            &resumed.member_id,
            None,
            directory,
            None,
        ) {
            warn!(member_id = resumed.member_id, %err, "skipping suspended Herdr connection that could not be activated");
            continue;
        }
        return Ok(Some(PendingFederationActivation {
            endpoint_id: resumed.member_id.clone(),
            expected_runtime_identity: resumed.server_id.clone().zip(resumed.session_id.clone()),
            connection: resumed,
            expected_resource: None,
            request_id,
            retain_current: false,
            restore_if_rejected: false,
            deadline: Instant::now() + FEDERATION_ACTIVATION_TIMEOUT,
            buffered_messages: Vec::new(),
        }));
    }
    Ok(None)
}

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread → sends raw input bytes to main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
async fn run_client_loop(
    stream: LocalStream,
    cols: u16,
    rows: u16,
    should_quit: Arc<AtomicBool>,
    config: ClientLoopConfig,
    requested_encoding: RenderEncoding,
    negotiated_encoding: RenderEncoding,
    attach_request: Option<(String, bool)>,
    attach_escape: Option<AttachEscapeState>,
    local_member_id: String,
) -> Result<(), ClientError> {
    #[cfg(windows)]
    let _ = config.mouse_scroll_lines;
    let draw_host_cursor = attach_escape.is_none() && should_draw_host_cursor(config.host_cursor);
    #[cfg(unix)]
    let mut is_remote_client = is_remote_client_process();

    let mut state = ClientState {
        blit_encoder: render_ansi::BlitEncoder::new(),
        mouse_capture_active: config.mouse_capture_active,
        reported_size: (cols, rows),
        sound_config: config.sound_config,
        kitty_graphics_enabled: config.kitty_graphics_enabled,
        attach_escape,
        #[cfg(unix)]
        mouse_scroll_lines: config.mouse_scroll_lines,
        #[cfg(unix)]
        remote_image_paste_key: config.remote_image_paste_key,
        redraw_on_focus_gained: config.redraw_on_focus_gained,
        draw_host_cursor,
        #[cfg(unix)]
        last_semantic_frame: None,
        #[cfg(unix)]
        federation_connecting: None,
        #[cfg(unix)]
        palette: config.palette,
    };
    debug!(?negotiated_encoding, "client render encoding active");
    let host_mouse_capture_active = Arc::new(AtomicBool::new(state.mouse_capture_active));

    // Channel for events from the stdin, resize, and server reader threads.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);

    // Spawn the stdin reader thread.
    let will_query_host_terminal_theme =
        state.attach_escape.is_none() && should_query_host_terminal_theme();
    let stdin_quit = should_quit.clone();
    let stdin_tx = event_tx.clone();
    let stdin_mouse_capture_active = host_mouse_capture_active.clone();
    let _input_thread = std::thread::spawn(move || {
        input::stdin_reader_loop(
            stdin_tx,
            &stdin_quit,
            will_query_host_terminal_theme,
            stdin_mouse_capture_active,
        );
    });

    if will_query_host_terminal_theme {
        query_host_terminal_theme();
    }

    // Spawn the resize poller thread.
    let resize_quit = should_quit.clone();
    let resize_tx = event_tx.clone();
    let kitty_graphics_enabled = state.kitty_graphics_enabled;
    std::thread::spawn(move || {
        resize_poll_loop(resize_tx, cols, rows, kitty_graphics_enabled, &resize_quit);
    });

    // Spawn the server reader thread (blocking reads from the socket).
    // Clone the stream's file descriptor so we can read from a blocking stream.
    let mut active_connection_id = 1_u64;
    let mut next_connection_id = 2_u64;
    spawn_server_reader(
        &stream,
        &event_tx,
        &should_quit,
        kitty_graphics_enabled,
        active_connection_id,
    )?;

    // Use the original stream for writing (blocking is fine since we write
    // from the async loop).
    let mut write_stream = stream;
    write_stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;
    #[cfg(unix)]
    let mut active_remote_connection: Option<crate::remote::InPlaceRemoteConnection> = None;
    #[cfg(unix)]
    let mut suspended_connections = Vec::<SuspendedClientConnection>::new();
    #[cfg(unix)]
    let mut disconnected_connections = HashSet::<u64>::new();
    #[cfg(unix)]
    let mut active_member_id = local_member_id.clone();
    #[cfg(unix)]
    let mut active_server_id: Option<String> = None;
    #[cfg(unix)]
    let mut active_session_id: Option<String> = None;
    #[cfg(unix)]
    let mut active_window_title: Option<String> = None;
    #[cfg(unix)]
    let mut active_prefix_input_source_active = false;
    #[cfg(unix)]
    let mut directory_authority_member_id = local_member_id;
    #[cfg(unix)]
    let mut federation_directory = Vec::<crate::federation::EndpointState>::new();
    #[cfg(unix)]
    let mut pending_federation_activation: Option<PendingFederationActivation> = None;
    #[cfg(unix)]
    let mut next_activation_request_id = 1_u64;
    #[cfg(unix)]
    let mut directory_authority_connection_id = 1_u64;
    #[cfg(unix)]
    let mut pending_directory_authority_connection_id: Option<u64> = None;
    #[cfg(unix)]
    let mut next_directory_authority_reconnect_at: Option<Instant> = None;
    #[cfg(windows)]
    let _ = local_member_id;

    // This (foreground) client owns the prefix ASCII input-source switch; a no-op on non-macOS.
    use crate::platform::PrefixInputSource;
    let mut prefix_input_source = crate::platform::RealPrefixInputSource::default();

    macro_rules! handle_active_write_failure {
        ($error:expr) => {{
            let error = $error;
            #[cfg(unix)]
            {
                if let Some(pending) = pending_federation_activation.as_mut() {
                    pending.retain_current = false;
                    warn!(%error, "source write failed while destination activation is pending");
                    continue;
                }
                pending_federation_activation = begin_suspended_fallback(
                    &mut suspended_connections,
                    &mut disconnected_connections,
                    &mut next_connection_id,
                    &mut next_activation_request_id,
                    requested_encoding,
                    attach_request.is_some(),
                    state.kitty_graphics_enabled,
                    &event_tx,
                    &should_quit,
                    &federation_directory,
                )?;
                if pending_federation_activation.is_some() {
                    warn!(%error, "active Herdr write failed; activating a retained member");
                    continue;
                }
            }
            return Err(ClientError::ConnectionLost(error));
        }};
    }

    // Main event loop.
    while !should_quit.load(Ordering::Acquire) {
        let event = tokio::select! {
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
            _ = tokio::time::sleep(Duration::from_millis(100)) => ClientLoopEvent::Timer,
        };

        match event {
            #[cfg(unix)]
            ClientLoopEvent::StdinInput(data) => {
                let data = if let Some(attach_escape) = &mut state.attach_escape {
                    match attach_escape.filter_input(
                        data,
                        state.reported_size.1,
                        state.mouse_scroll_lines,
                    ) {
                        AttachInputAction::Forward(data) => data,
                        AttachInputAction::Scroll {
                            source,
                            direction,
                            lines,
                            column,
                            row,
                            modifiers,
                        } => {
                            let msg = ClientMessage::AttachScroll {
                                source,
                                direction,
                                lines,
                                column,
                                row,
                                modifiers,
                            };
                            if let Err(e) = write_to_server(&mut write_stream, &msg) {
                                handle_active_write_failure!(e);
                            }
                            continue;
                        }
                        AttachInputAction::Detach => {
                            let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
                            return Ok(());
                        }
                        AttachInputAction::None => continue,
                    }
                } else {
                    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                    if crate::raw_input::events_require_host_surface_redraw(
                        &events,
                        state.redraw_on_focus_gained,
                    ) {
                        state.request_full_redraw();
                    }
                    if crate::raw_input::events_require_host_terminal_theme_query(&events) {
                        query_host_terminal_theme();
                    }
                    data
                };
                if should_bridge_clipboard_image_paste(
                    &data,
                    is_remote_client,
                    state.remote_image_paste_key,
                ) {
                    if let Some(image) = crate::platform::read_clipboard_image() {
                        if image.bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                            warn!(
                                bytes = image.bytes.len(),
                                max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                                "local clipboard image is too large to bridge"
                            );
                            continue;
                        }
                        info!(
                            bytes = image.bytes.len(),
                            extension = image.extension,
                            "bridging local clipboard image paste to remote server"
                        );
                        let msg = ClientMessage::ClipboardImage {
                            extension: image.extension.to_owned(),
                            data: image.bytes,
                        };
                        if let Err(e) = write_to_server(&mut write_stream, &msg) {
                            handle_active_write_failure!(e);
                        }
                        continue;
                    }
                    info!(
                        "clipboard image paste trigger received, but local clipboard has no image"
                    );
                }
                if let Some(image) = read_image_file_from_terminal_drop(&data, is_remote_client) {
                    info!(
                        bytes = image.bytes.len(),
                        extension = image.extension,
                        "bridging local image file drop to remote server"
                    );
                    let msg = ClientMessage::ClipboardImage {
                        extension: image.extension.to_owned(),
                        data: image.bytes,
                    };
                    if let Err(e) = write_to_server(&mut write_stream, &msg) {
                        handle_active_write_failure!(e);
                    }
                    continue;
                }
                let msg = ClientMessage::Input { data };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    handle_active_write_failure!(e);
                }
            }
            #[cfg(windows)]
            ClientLoopEvent::StdinEvents(events) => {
                if state.attach_escape.is_some() {
                    continue;
                }
                let raw_events = events
                    .iter()
                    .map(crate::protocol::ClientInputEvent::to_raw_input_event)
                    .collect::<Vec<_>>();
                if crate::raw_input::events_require_host_surface_redraw(
                    &raw_events,
                    state.redraw_on_focus_gained,
                ) {
                    state.request_full_redraw();
                }
                let msg = ClientMessage::InputEvents { events };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    handle_active_write_failure!(e);
                }
            }
            ClientLoopEvent::Resize(new_cols, new_rows, cell_width_px, cell_height_px) => {
                state.reported_size = (new_cols, new_rows);
                let msg = ClientMessage::Resize {
                    cols: new_cols,
                    rows: new_rows,
                    cell_width_px,
                    cell_height_px,
                };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    handle_active_write_failure!(e);
                }
            }
            ClientLoopEvent::ServerMessage {
                connection_id,
                message,
            } => {
                #[cfg(unix)]
                if let ServerMessage::FederationDirectoryUpdate { directory } = &message {
                    if connection_id == directory_authority_connection_id {
                        merge_federation_directory(&mut federation_directory, directory.clone());
                        if active_connection_id != directory_authority_connection_id {
                            let _ = write_to_server(
                                &mut write_stream,
                                &ClientMessage::FederationDirectoryUpdate {
                                    directory: federation_directory.clone(),
                                },
                            );
                        }
                        state.request_full_redraw();
                    } else {
                        warn!(
                            connection_id,
                            "ignoring federation directory update from non-home member"
                        );
                    }
                    continue;
                }
                #[cfg(unix)]
                if let ServerMessage::FederationIdentity {
                    member_id,
                    server_id,
                    session_id,
                } = &message
                {
                    if pending_directory_authority_connection_id == Some(connection_id) {
                        let identity_matches = suspended_connections.iter().any(|connection| {
                            connection.connection_id == connection_id
                                && directory_authority_identity_matches(
                                    &connection.member_id,
                                    connection.server_id.as_deref(),
                                    connection.session_id.as_deref(),
                                    &directory_authority_member_id,
                                    member_id,
                                    server_id,
                                    session_id,
                                )
                        });
                        pending_directory_authority_connection_id = None;
                        if identity_matches {
                            directory_authority_connection_id = connection_id;
                            disconnected_connections.remove(&connection_id);
                            next_directory_authority_reconnect_at = None;
                            info!(
                                connection_id,
                                member_id, "restored suspended home directory authority"
                            );
                        } else {
                            disconnected_connections.insert(connection_id);
                            next_directory_authority_reconnect_at =
                                Some(Instant::now() + DIRECTORY_AUTHORITY_RECONNECT_INTERVAL);
                            warn!(
                                connection_id,
                                member_id,
                                server_id,
                                session_id,
                                "rejected replacement home directory authority identity"
                            );
                        }
                        continue;
                    }
                    if connection_id == directory_authority_connection_id {
                        directory_authority_member_id.clone_from(member_id);
                        if connection_id == active_connection_id {
                            active_member_id.clone_from(member_id);
                            active_server_id = Some(server_id.clone());
                            active_session_id = Some(session_id.clone());
                        }
                    }
                    continue;
                }
                #[cfg(unix)]
                if let ServerMessage::FederationActivationResult {
                    request_id,
                    accepted,
                    member_id,
                    server_id,
                    session_id,
                    error,
                } = &message
                {
                    let is_pending =
                        pending_federation_activation
                            .as_ref()
                            .is_some_and(|pending| {
                                pending.connection.connection_id == connection_id
                                    && pending.request_id == *request_id
                            });
                    if is_pending {
                        let mut pending = pending_federation_activation
                            .take()
                            .expect("pending activation checked above");
                        let identity_matches = federation_activation_identity_matches(
                            &pending.endpoint_id,
                            pending.expected_resource.as_ref(),
                            pending.expected_runtime_identity.as_ref(),
                            member_id,
                            server_id,
                            session_id,
                        );
                        if *accepted && identity_matches {
                            if pending.retain_current {
                                if let Err(err) = write_to_server(
                                    &mut write_stream,
                                    &ClientMessage::FederationSuspend,
                                ) {
                                    warn!(%err, "previous Herdr connection closed while destination activation succeeded; committing destination");
                                    pending.retain_current = false;
                                }
                            }
                            let ResumedClientConnection {
                                member_id: resumed_member_id,
                                server_id: _,
                                session_id: _,
                                stream: resumed_stream,
                                connection_id: resumed_id,
                                remote: resumed_remote,
                                mouse_capture_active: resumed_mouse_capture,
                                window_title: resumed_window_title,
                                prefix_input_source_active: resumed_prefix_input_source_active,
                                is_remote_client: resumed_is_remote,
                                reconnected_encoding,
                            } = pending.connection;
                            let previous_stream =
                                std::mem::replace(&mut write_stream, resumed_stream);
                            if pending.retain_current {
                                suspended_connections.push(SuspendedClientConnection {
                                    member_id: active_member_id,
                                    server_id: active_server_id,
                                    session_id: active_session_id,
                                    stream: previous_stream,
                                    connection_id: active_connection_id,
                                    remote: active_remote_connection.take(),
                                    mouse_capture_active: state.mouse_capture_active,
                                    window_title: active_window_title,
                                    prefix_input_source_active: active_prefix_input_source_active,
                                    is_remote_client,
                                });
                            } else {
                                drop(previous_stream);
                                drop(active_remote_connection.take());
                            }
                            active_member_id = resumed_member_id;
                            active_server_id = Some(server_id.clone());
                            active_session_id = Some(session_id.clone());
                            if active_member_id == directory_authority_member_id {
                                directory_authority_connection_id = resumed_id;
                            }
                            active_remote_connection = resumed_remote;
                            active_connection_id = resumed_id;
                            is_remote_client = resumed_is_remote;
                            if state.mouse_capture_active != resumed_mouse_capture {
                                set_mouse_capture(resumed_mouse_capture)
                                    .map_err(ClientError::ConnectionFailed)?;
                                state.mouse_capture_active = resumed_mouse_capture;
                                host_mouse_capture_active
                                    .store(resumed_mouse_capture, Ordering::Release);
                            }
                            write_window_title(resumed_window_title.as_deref());
                            if resumed_prefix_input_source_active {
                                prefix_input_source.switch_to_ascii();
                            } else {
                                prefix_input_source.restore();
                            }
                            active_window_title = resumed_window_title;
                            active_prefix_input_source_active = resumed_prefix_input_source_active;
                            clear_connecting_ui(&mut state, false);
                            state.request_full_redraw();
                            let (cols, rows, cell_width_px, cell_height_px) =
                                current_terminal_geometry(state.kitty_graphics_enabled);
                            let _ = write_to_server(
                                &mut write_stream,
                                &ClientMessage::Resize {
                                    cols,
                                    rows,
                                    cell_width_px,
                                    cell_height_px,
                                },
                            );
                            for buffered in pending.buffered_messages.drain(..) {
                                let _ = event_tx.try_send(ClientLoopEvent::ServerMessage {
                                    connection_id: active_connection_id,
                                    message: buffered,
                                });
                            }
                            info!(
                                member_id,
                                server_id,
                                session_id,
                                ?reconnected_encoding,
                                connection_id = resumed_id,
                                "federation member activation accepted"
                            );
                        } else {
                            clear_connecting_ui(&mut state, pending.retain_current);
                            let detail = error.clone().unwrap_or_else(|| {
                                format!(
                                    "receiver identity was {member_id}/{server_id}/{session_id}"
                                )
                            });
                            let _ = write_to_server(
                                &mut pending.connection.stream,
                                &ClientMessage::FederationSuspend,
                            );
                            if pending.restore_if_rejected && member_id == &pending.endpoint_id {
                                let connection = pending.connection;
                                suspended_connections.push(SuspendedClientConnection {
                                    member_id: connection.member_id,
                                    server_id: connection.server_id,
                                    session_id: connection.session_id,
                                    stream: connection.stream,
                                    connection_id: connection.connection_id,
                                    remote: connection.remote,
                                    mouse_capture_active: connection.mouse_capture_active,
                                    window_title: connection.window_title,
                                    prefix_input_source_active: connection
                                        .prefix_input_source_active,
                                    is_remote_client: connection.is_remote_client,
                                });
                            }
                            warn!(endpoint_id = pending.endpoint_id, %detail, "federation activation rejected");
                            handle_notify(
                                NotifyKind::Toast,
                                "Federated member rejected activation",
                                Some(&format!("{}: {detail}", pending.endpoint_id)),
                                &state.sound_config,
                            );
                            if !pending.retain_current {
                                pending_federation_activation = begin_suspended_fallback(
                                    &mut suspended_connections,
                                    &mut disconnected_connections,
                                    &mut next_connection_id,
                                    &mut next_activation_request_id,
                                    requested_encoding,
                                    attach_request.is_some(),
                                    state.kitty_graphics_enabled,
                                    &event_tx,
                                    &should_quit,
                                    &federation_directory,
                                )?;
                                if pending_federation_activation.is_none() {
                                    return Err(ClientError::ConnectionLost(io::Error::other(
                                        "no healthy suspended Herdr connection remains",
                                    )));
                                }
                            }
                        }
                        continue;
                    }
                }
                #[cfg(unix)]
                if pending_federation_activation
                    .as_ref()
                    .is_some_and(|pending| pending.connection.connection_id == connection_id)
                {
                    if matches!(
                        message,
                        ServerMessage::MouseCapture { .. }
                            | ServerMessage::WindowTitle { .. }
                            | ServerMessage::PrefixInputSource { .. }
                            | ServerMessage::ReloadSoundConfig
                            | ServerMessage::Clipboard { .. }
                            | ServerMessage::Notify { .. }
                    ) {
                        if let Some(pending) = pending_federation_activation.as_mut() {
                            pending.buffered_messages.push(message);
                        }
                    }
                    continue;
                }
                if connection_id != active_connection_id {
                    #[cfg(unix)]
                    if matches!(message, ServerMessage::ServerShutdown { .. }) {
                        disconnected_connections.insert(connection_id);
                    }
                    continue;
                }
                match message {
                    ServerMessage::Frame(frame_data) => {
                        let frame_data = if state.draw_host_cursor {
                            render_ansi::frame_with_drawn_cursor(frame_data)
                        } else {
                            frame_data
                        };
                        #[cfg(unix)]
                        display_semantic_frame(&mut state, frame_data, true);
                        #[cfg(windows)]
                        {
                            let encoded = if state.draw_host_cursor {
                                state
                                    .blit_encoder
                                    .encode_with_suppressed_visible_cursor(&frame_data, false)
                            } else {
                                state.blit_encoder.encode(&frame_data, false)
                            };
                            let mut stdout = io::stdout();
                            let graphics = if state.kitty_graphics_enabled {
                                frame_data.graphics.as_slice()
                            } else {
                                &[]
                            };
                            let _ = write_encoded_frame_with_graphics(
                                &mut stdout,
                                &encoded.bytes,
                                graphics,
                            );
                            let _ = stdout.flush();
                            state.blit_encoder.commit(frame_data, encoded);
                        }
                    }
                    ServerMessage::Terminal(frame) => {
                        if state.kitty_graphics_enabled
                            && contains_kitty_graphics_bytes(&frame.bytes)
                        {
                            record_received_kitty_graphics(&frame.bytes);
                        }
                        let mut stdout = io::stdout();
                        let _ = stdout.write_all(&frame.bytes);
                        let _ = stdout.flush();
                    }
                    ServerMessage::Graphics { bytes } => {
                        if state.kitty_graphics_enabled {
                            record_received_kitty_graphics(&bytes);
                            let mut stdout = io::stdout();
                            let _ = stdout.write_all(&bytes);
                            let _ = stdout.flush();
                        }
                    }
                    ServerMessage::ServerShutdown { reason } => {
                        if reason.as_deref() == Some(crate::protocol::LIVE_HANDOFF_RECONNECT_REASON)
                        {
                            #[cfg(unix)]
                            let reconnect_result =
                                if let Some(remote) = active_remote_connection.as_ref() {
                                    reconnect_remote_after_live_handoff(
                                        remote,
                                        requested_encoding,
                                        attach_request.is_some(),
                                        state.kitty_graphics_enabled,
                                    )
                                } else {
                                    reconnect_after_live_handoff(
                                        requested_encoding,
                                        attach_request.is_some(),
                                        attach_request.as_ref(),
                                        state.kitty_graphics_enabled,
                                    )
                                };
                            #[cfg(not(unix))]
                            let reconnect_result = reconnect_after_live_handoff(
                                requested_encoding,
                                attach_request.is_some(),
                                attach_request.as_ref(),
                                state.kitty_graphics_enabled,
                            );
                            match reconnect_result {
                                Ok((reconnected_stream, reconnected_encoding)) => {
                                    #[cfg(unix)]
                                    {
                                        let mut reconnected_stream = reconnected_stream;
                                        let candidate_connection_id = next_connection_id;
                                        next_connection_id = next_connection_id.wrapping_add(1);
                                        spawn_server_reader(
                                            &reconnected_stream,
                                            &event_tx,
                                            &should_quit,
                                            state.kitty_graphics_enabled,
                                            candidate_connection_id,
                                        )?;
                                        let request_id = next_activation_request_id;
                                        next_activation_request_id =
                                            next_activation_request_id.wrapping_add(1);
                                        activate_federation_connection(
                                            &mut reconnected_stream,
                                            request_id,
                                            &active_member_id,
                                            None,
                                            &federation_directory,
                                            None,
                                        )
                                        .map_err(ClientError::ConnectionLost)?;
                                        pending_federation_activation =
                                            Some(PendingFederationActivation {
                                                connection: ResumedClientConnection {
                                                    member_id: active_member_id.clone(),
                                                    server_id: active_server_id.clone(),
                                                    session_id: active_session_id.clone(),
                                                    stream: reconnected_stream,
                                                    connection_id: candidate_connection_id,
                                                    remote: active_remote_connection.take(),
                                                    mouse_capture_active: state
                                                        .mouse_capture_active,
                                                    window_title: active_window_title.clone(),
                                                    prefix_input_source_active:
                                                        active_prefix_input_source_active,
                                                    is_remote_client,
                                                    reconnected_encoding: Some(
                                                        reconnected_encoding,
                                                    ),
                                                },
                                                endpoint_id: active_member_id.clone(),
                                                expected_resource: None,
                                                expected_runtime_identity: active_server_id
                                                    .clone()
                                                    .zip(active_session_id.clone()),
                                                request_id,
                                                retain_current: false,
                                                restore_if_rejected: false,
                                                deadline: Instant::now()
                                                    + FEDERATION_ACTIVATION_TIMEOUT,
                                                buffered_messages: Vec::new(),
                                            });
                                        info!("waiting for replacement Herdr identity acknowledgement");
                                        continue;
                                    }
                                    #[cfg(not(unix))]
                                    {
                                        active_connection_id = next_connection_id;
                                        next_connection_id = next_connection_id.wrapping_add(1);
                                        spawn_server_reader(
                                            &reconnected_stream,
                                            &event_tx,
                                            &should_quit,
                                            state.kitty_graphics_enabled,
                                            active_connection_id,
                                        )?;
                                        write_stream = reconnected_stream;
                                    }
                                    #[cfg(not(unix))]
                                    {
                                        state.request_full_redraw();
                                        info!(
                                            ?reconnected_encoding,
                                            "client reconnected after live handoff"
                                        );
                                        continue;
                                    }
                                }
                                #[cfg(unix)]
                                Err(err) if !suspended_connections.is_empty() => {
                                    warn!(%err, "remote live handoff reconnect failed; resuming previous Herdr connection");
                                    handle_notify(
                                        NotifyKind::Toast,
                                        "Remote Herdr connection changed",
                                        Some("Returning to the previous Herdr connection without restarting the client."),
                                        &state.sound_config,
                                    );
                                }
                                Err(err) => return Err(err),
                            }
                        }
                        #[cfg(unix)]
                        if reason.as_deref() != Some("detached") {
                            if let Some(pending) = pending_federation_activation.as_mut() {
                                pending.retain_current = false;
                                warn!(?reason, "source Herdr closed while destination activation is pending; waiting for destination acknowledgement");
                                continue;
                            }
                            pending_federation_activation = begin_suspended_fallback(
                                &mut suspended_connections,
                                &mut disconnected_connections,
                                &mut next_connection_id,
                                &mut next_activation_request_id,
                                requested_encoding,
                                attach_request.is_some(),
                                state.kitty_graphics_enabled,
                                &event_tx,
                                &should_quit,
                                &federation_directory,
                            )?;
                            if pending_federation_activation.is_some() {
                                handle_notify(
                                    NotifyKind::Toast,
                                    "Herdr member disconnected",
                                    Some("Activating a retained member without restarting the client."),
                                    &state.sound_config,
                                );
                                state.request_full_redraw();
                                continue;
                            }
                        }
                        return Err(ClientError::ServerShutdown { reason });
                    }
                    ServerMessage::Notify {
                        kind,
                        message,
                        body,
                    } => {
                        handle_notify(kind, &message, body.as_deref(), &state.sound_config);
                    }
                    ServerMessage::Clipboard { data } => {
                        forward_clipboard(&data);
                        let _ = io::stdout().flush();
                    }
                    ServerMessage::WindowTitle { title } => {
                        #[cfg(unix)]
                        active_window_title.clone_from(&title);
                        write_window_title(title.as_deref());
                        let _ = io::stdout().flush();
                    }
                    ServerMessage::ReloadSoundConfig => {
                        reload_local_client_config(
                            &mut state.sound_config,
                            &mut state.redraw_on_focus_gained,
                            &mut state.draw_host_cursor,
                            #[cfg(unix)]
                            &mut state.remote_image_paste_key,
                        );
                    }
                    ServerMessage::MouseCapture { enabled } => {
                        let desired = enabled;
                        if desired != state.mouse_capture_active {
                            set_mouse_capture(desired).map_err(ClientError::ConnectionFailed)?;
                            #[cfg(windows)]
                            if windows_vti_input_backend_enabled() {
                                let _ = enable_windows_virtual_terminal_input();
                            }
                            state.mouse_capture_active = desired;
                            host_mouse_capture_active.store(desired, Ordering::Release);
                        }
                    }
                    ServerMessage::PrefixInputSource { active } => {
                        #[cfg(unix)]
                        {
                            active_prefix_input_source_active = active;
                        }
                        if active {
                            prefix_input_source.switch_to_ascii();
                        } else {
                            prefix_input_source.restore();
                        }
                    }
                    ServerMessage::FederationAttach {
                        endpoint_id,
                        target: _requested_target,
                        session: _requested_session,
                        resource,
                        directory,
                        presentation,
                    } => {
                        #[cfg(unix)]
                        {
                            let resource = resource.map(|resource| *resource);
                            if connection_id == directory_authority_connection_id {
                                merge_federation_directory(&mut federation_directory, directory);
                            }
                            let Some(authoritative) = federation_directory
                                .iter()
                                .find(|state| state.endpoint.id == endpoint_id)
                            else {
                                warn!(
                                    endpoint_id,
                                    "ignoring unpaired federation endpoint selection"
                                );
                                handle_notify(
                                    NotifyKind::Toast,
                                    "Untrusted federation destination",
                                    Some("The selected member is not present in the pinned home directory."),
                                    &state.sound_config,
                                );
                                continue;
                            };
                            if authoritative.snapshot.is_none() {
                                warn!(endpoint_id, "ignoring federation endpoint without an authoritative runtime snapshot");
                                handle_notify(
                                    NotifyKind::Toast,
                                    "Federated member unavailable",
                                    Some("The home directory has no verified runtime identity for this member."),
                                    &state.sound_config,
                                );
                                continue;
                            }
                            let expected_runtime_identity =
                                federated_endpoint_runtime_identity(authoritative)
                                    .expect("authoritative snapshot checked above");
                            if let Some(resource) = resource.as_ref() {
                                let identity_matches = resource.endpoint_id == endpoint_id
                                    && expected_runtime_identity.0 == resource.server_id
                                    && expected_runtime_identity.1 == resource.session_id;
                                if !identity_matches {
                                    warn!(
                                        endpoint_id,
                                        "ignoring stale or untrusted qualified resource"
                                    );
                                    handle_notify(
                                        NotifyKind::Toast,
                                        "Stale federated resource",
                                        Some("Refresh the home directory before retrying this selection."),
                                        &state.sound_config,
                                    );
                                    continue;
                                }
                            }
                            let target = authoritative.endpoint.target.clone();
                            let session = authoritative.endpoint.session.clone();
                            let member_label = authoritative
                                .endpoint
                                .label
                                .clone()
                                .unwrap_or_else(|| endpoint_id.clone());
                            if pending_federation_activation.is_some() {
                                handle_notify(
                                    NotifyKind::Toast,
                                    "Federation switch already in progress",
                                    Some("Wait for the selected member to accept or reject activation."),
                                    &state.sound_config,
                                );
                                continue;
                            }
                            let plan = federation_connection_plan(
                                &active_member_id,
                                suspended_connections
                                    .iter()
                                    .map(|connection| connection.member_id.as_str()),
                                &endpoint_id,
                            );

                            if plan == FederationConnectionPlan::Current {
                                let request_id = next_activation_request_id;
                                next_activation_request_id =
                                    next_activation_request_id.wrapping_add(1);
                                if let Err(err) = activate_federation_connection(
                                    &mut write_stream,
                                    request_id,
                                    &endpoint_id,
                                    resource,
                                    &federation_directory,
                                    Some(presentation),
                                ) {
                                    return Err(ClientError::ConnectionLost(err));
                                }
                                state.request_full_redraw();
                                continue;
                            }

                            state.federation_connecting =
                                FederationConnectingUi::for_plan(plan, &member_label);
                            redraw_connecting_ui(&mut state);

                            let switch_result = match plan {
                                FederationConnectionPlan::Suspended(index) => {
                                    let suspended = suspended_connections.remove(index);
                                    let reconnect =
                                        disconnected_connections.remove(&suspended.connection_id);
                                    resume_suspended_connection(
                                        suspended,
                                        reconnect,
                                        next_connection_id,
                                        requested_encoding,
                                        attach_request.is_some(),
                                        state.kitty_graphics_enabled,
                                    )
                                    .map(|resumed| (resumed, reconnect))
                                    .map_err(|err| io::Error::other(err.to_string()))
                                }
                                FederationConnectionPlan::New => (|| {
                                    let remote = crate::remote::start_in_place_remote_connection(
                                        target.clone(),
                                        session,
                                    )?;
                                    let stream = remote.connect()?;
                                    let (stream, encoding) = handshake_current_terminal(
                                        stream,
                                        requested_encoding,
                                        false,
                                        state.kitty_graphics_enabled,
                                        true,
                                    )
                                    .map_err(|err| io::Error::other(err.to_string()))?;
                                    Ok((
                                        ResumedClientConnection {
                                            member_id: endpoint_id.clone(),
                                            server_id: Some(expected_runtime_identity.0.clone()),
                                            session_id: Some(expected_runtime_identity.1.clone()),
                                            stream,
                                            connection_id: next_connection_id,
                                            remote: Some(remote),
                                            mouse_capture_active: state.mouse_capture_active,
                                            window_title: None,
                                            prefix_input_source_active: false,
                                            is_remote_client: true,
                                            reconnected_encoding: Some(encoding),
                                        },
                                        true,
                                    ))
                                })(
                                ),
                                FederationConnectionPlan::Current => unreachable!(),
                            };

                            match switch_result {
                                Ok((mut resumed, needs_reader)) => {
                                    if needs_reader {
                                        next_connection_id = next_connection_id.wrapping_add(1);
                                        spawn_server_reader(
                                            &resumed.stream,
                                            &event_tx,
                                            &should_quit,
                                            state.kitty_graphics_enabled,
                                            resumed.connection_id,
                                        )?;
                                    }
                                    let request_id = next_activation_request_id;
                                    next_activation_request_id =
                                        next_activation_request_id.wrapping_add(1);
                                    if let Err(err) = activate_federation_connection(
                                        &mut resumed.stream,
                                        request_id,
                                        &endpoint_id,
                                        resource.clone(),
                                        &federation_directory,
                                        Some(presentation),
                                    ) {
                                        clear_connecting_ui(&mut state, true);
                                        warn!(endpoint_id, target, %err, "federation activation request failed; keeping current connection");
                                        handle_notify(
                                            NotifyKind::Toast,
                                            "Could not open remote Herdr",
                                            Some(&format!(
                                                "{endpoint_id}: {err}. The current Herdr connection is still active."
                                            )),
                                            &state.sound_config,
                                        );
                                        continue;
                                    }
                                    let resumed_id = resumed.connection_id;
                                    let reconnected_encoding = resumed.reconnected_encoding;
                                    pending_federation_activation =
                                        Some(PendingFederationActivation {
                                            connection: resumed,
                                            endpoint_id: endpoint_id.clone(),
                                            expected_runtime_identity: Some(
                                                expected_runtime_identity,
                                            ),
                                            expected_resource: resource,
                                            request_id,
                                            retain_current: true,
                                            restore_if_rejected: matches!(
                                                plan,
                                                FederationConnectionPlan::Suspended(_)
                                            ),
                                            deadline: Instant::now()
                                                + FEDERATION_ACTIVATION_TIMEOUT,
                                            buffered_messages: Vec::new(),
                                        });
                                    info!(
                                        endpoint_id,
                                        target,
                                        reused =
                                            matches!(plan, FederationConnectionPlan::Suspended(_)),
                                        ?reconnected_encoding,
                                        connection_id = resumed_id,
                                        "waiting for Herdr member activation acknowledgement"
                                    );
                                }
                                Err(err) => {
                                    clear_connecting_ui(&mut state, true);
                                    warn!(endpoint_id, target, %err, "federation connection switch failed; keeping current connection");
                                    handle_notify(
                                        NotifyKind::Toast,
                                        "Could not open remote Herdr",
                                        Some(&format!(
                                            "{endpoint_id}: {err}. The current Herdr connection is still active."
                                        )),
                                        &state.sound_config,
                                    );
                                    state.request_full_redraw();
                                }
                            }
                        }
                        #[cfg(windows)]
                        {
                            let _ = (
                                endpoint_id,
                                _requested_target,
                                _requested_session,
                                resource,
                                directory,
                            );
                            handle_notify(
                                NotifyKind::Toast,
                                "Remote Herdr attachment is unavailable",
                                Some("In-place federation attachment is not supported on Windows."),
                                &state.sound_config,
                            );
                        }
                    }
                    ServerMessage::FederationActivationResult {
                        accepted,
                        member_id,
                        error,
                        ..
                    } => {
                        if !accepted {
                            let detail = error.unwrap_or_else(|| {
                                format!("member {member_id} rejected activation")
                            });
                            handle_notify(
                                NotifyKind::Toast,
                                "Federated member rejected activation",
                                Some(&detail),
                                &state.sound_config,
                            );
                        }
                    }
                    ServerMessage::FederationIdentity { .. }
                    | ServerMessage::FederationDirectoryUpdate { .. } => {
                        // Handled before active-connection routing so the
                        // pinned home connection may update while suspended.
                    }
                    ServerMessage::Welcome { .. } => {
                        debug!("received unexpected Welcome in main loop");
                    }
                }
            }
            ClientLoopEvent::ServerDisconnected { connection_id } => {
                #[cfg(unix)]
                if pending_federation_activation
                    .as_ref()
                    .is_some_and(|pending| pending.connection.connection_id == connection_id)
                {
                    let pending = pending_federation_activation
                        .take()
                        .expect("pending connection id checked above");
                    clear_connecting_ui(&mut state, pending.retain_current);
                    warn!(
                        endpoint_id = pending.endpoint_id,
                        "pending federation activation disconnected"
                    );
                    handle_notify(
                        NotifyKind::Toast,
                        "Federated member disconnected",
                        Some("The current Herdr connection remains active."),
                        &state.sound_config,
                    );
                    if !pending.retain_current {
                        pending_federation_activation = begin_suspended_fallback(
                            &mut suspended_connections,
                            &mut disconnected_connections,
                            &mut next_connection_id,
                            &mut next_activation_request_id,
                            requested_encoding,
                            attach_request.is_some(),
                            state.kitty_graphics_enabled,
                            &event_tx,
                            &should_quit,
                            &federation_directory,
                        )?;
                        if pending_federation_activation.is_none() {
                            return Err(ClientError::ConnectionLost(io::Error::other(
                                "no healthy suspended Herdr connection remains",
                            )));
                        }
                    }
                    continue;
                }
                if connection_id != active_connection_id {
                    #[cfg(unix)]
                    {
                        disconnected_connections.insert(connection_id);
                        if connection_id == directory_authority_connection_id
                            || pending_directory_authority_connection_id == Some(connection_id)
                        {
                            pending_directory_authority_connection_id = None;
                            next_directory_authority_reconnect_at = Some(Instant::now());
                        }
                    }
                    continue;
                }
                #[cfg(unix)]
                if let Some(pending) = pending_federation_activation.as_mut() {
                    pending.retain_current = false;
                    warn!("source Herdr disconnected while destination activation is pending; waiting for destination acknowledgement");
                    continue;
                }
                #[cfg(unix)]
                {
                    pending_federation_activation = begin_suspended_fallback(
                        &mut suspended_connections,
                        &mut disconnected_connections,
                        &mut next_connection_id,
                        &mut next_activation_request_id,
                        requested_encoding,
                        attach_request.is_some(),
                        state.kitty_graphics_enabled,
                        &event_tx,
                        &should_quit,
                        &federation_directory,
                    )?;
                    if pending_federation_activation.is_some() {
                        handle_notify(
                            NotifyKind::Toast,
                            "Remote Herdr disconnected",
                            Some("Activating a retained member without restarting the client."),
                            &state.sound_config,
                        );
                        continue;
                    }
                }
                return Err(ClientError::ConnectionLost(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed connection",
                )));
            }
            ClientLoopEvent::Timer => {
                #[cfg(unix)]
                if pending_federation_activation.is_some() && state.federation_connecting.is_some()
                {
                    if let Some(connecting) = state.federation_connecting.as_mut() {
                        connecting.tick = connecting.tick.wrapping_add(1);
                    }
                    redraw_connecting_ui(&mut state);
                }
                #[cfg(unix)]
                if active_connection_id != directory_authority_connection_id
                    && pending_directory_authority_connection_id.is_none()
                    && next_directory_authority_reconnect_at
                        .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    let authority_index = suspended_connections.iter().position(|connection| {
                        connection.member_id == directory_authority_member_id
                    });
                    if let Some(index) = authority_index {
                        let connection = &mut suspended_connections[index];
                        match reconnect_suspended_connection(
                            connection.remote.as_ref(),
                            requested_encoding,
                            attach_request.is_some(),
                            state.kitty_graphics_enabled,
                        ) {
                            Ok((stream, _encoding)) => {
                                let connection_id = next_connection_id;
                                next_connection_id = next_connection_id.wrapping_add(1);
                                spawn_server_reader(
                                    &stream,
                                    &event_tx,
                                    &should_quit,
                                    state.kitty_graphics_enabled,
                                    connection_id,
                                )?;
                                connection.stream = stream;
                                connection.connection_id = connection_id;
                                disconnected_connections.remove(&directory_authority_connection_id);
                                pending_directory_authority_connection_id = Some(connection_id);
                                next_directory_authority_reconnect_at = None;
                            }
                            Err(err) => {
                                warn!(%err, "suspended home directory authority is not ready; retrying");
                                next_directory_authority_reconnect_at =
                                    Some(Instant::now() + DIRECTORY_AUTHORITY_RECONNECT_INTERVAL);
                            }
                        }
                    } else {
                        next_directory_authority_reconnect_at = None;
                    }
                }
                #[cfg(unix)]
                if pending_federation_activation
                    .as_ref()
                    .is_some_and(|pending| Instant::now() >= pending.deadline)
                {
                    let mut pending = pending_federation_activation
                        .take()
                        .expect("expired activation checked above");
                    clear_connecting_ui(&mut state, pending.retain_current);
                    let _ = write_to_server(
                        &mut pending.connection.stream,
                        &ClientMessage::FederationSuspend,
                    );
                    warn!(
                        endpoint_id = pending.endpoint_id,
                        "federation activation acknowledgement timed out"
                    );
                    handle_notify(
                        NotifyKind::Toast,
                        "Federated member timed out",
                        Some("The current Herdr connection remains active."),
                        &state.sound_config,
                    );
                    if pending.restore_if_rejected {
                        let connection = pending.connection;
                        suspended_connections.push(SuspendedClientConnection {
                            member_id: connection.member_id,
                            server_id: connection.server_id,
                            session_id: connection.session_id,
                            stream: connection.stream,
                            connection_id: connection.connection_id,
                            remote: connection.remote,
                            mouse_capture_active: connection.mouse_capture_active,
                            window_title: connection.window_title,
                            prefix_input_source_active: connection.prefix_input_source_active,
                            is_remote_client: connection.is_remote_client,
                        });
                    }
                    if !pending.retain_current {
                        pending_federation_activation = begin_suspended_fallback(
                            &mut suspended_connections,
                            &mut disconnected_connections,
                            &mut next_connection_id,
                            &mut next_activation_request_id,
                            requested_encoding,
                            attach_request.is_some(),
                            state.kitty_graphics_enabled,
                            &event_tx,
                            &should_quit,
                            &federation_directory,
                        )?;
                        if pending_federation_activation.is_none() {
                            return Err(ClientError::ConnectionLost(io::Error::other(
                                "no healthy suspended Herdr connection remains",
                            )));
                        }
                    }
                }
            }
        }
    }

    // Clean exit (Ctrl+C). Send Detach before closing.
    let detach = ClientMessage::Detach;
    let _ = write_to_server(&mut write_stream, &detach);
    let _ = io::stdout().flush();

    Ok(())
}

// ---------------------------------------------------------------------------
// Server reader thread
// ---------------------------------------------------------------------------

/// Blocking thread that reads ServerMessages from the server and sends them
/// to the main event loop.
fn server_reader_thread(
    mut stream: LocalStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
    connection_id: u64,
) {
    // Ensure the read stream is in blocking mode to avoid WouldBlock errors
    // from read_exact inside read_message. The stream should already be
    // blocking after handshake, but we enforce it here as a safety measure.
    if stream.set_nonblocking(false).is_err() {
        // If we can't set blocking mode, the stream is likely broken.
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected { connection_id });
        return;
    }

    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, max_frame_size) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage {
                        connection_id,
                        message: msg,
                    })
                    .is_err()
                {
                    break; // Main loop gone.
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                // Server closed connection.
                let _ =
                    event_tx.blocking_send(ClientLoopEvent::ServerDisconnected { connection_id });
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                // Should not happen with blocking mode, but handle gracefully
                // in case the stream was set nonblocking by another clone.
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ =
                    event_tx.blocking_send(ClientLoopEvent::ServerDisconnected { connection_id });
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Write helper
// ---------------------------------------------------------------------------

/// Writes a message to the server stream (blocking).
fn write_to_server(stream: &mut LocalStream, msg: &ClientMessage) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|e| io::Error::other(e.to_string()))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn client_remote_image_paste_key(
    config: &crate::config::Config,
) -> Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)> {
    if !is_remote_client_process() {
        return None;
    }

    match config.remote_image_paste_key() {
        Ok(key) => key,
        Err(diagnostic) => {
            warn!(diagnostic = %diagnostic, "local remote image paste key config diagnostic");
            None
        }
    }
}

fn reload_local_client_config(
    sound_config: &mut crate::config::SoundConfig,
    redraw_on_focus_gained: &mut bool,
    draw_host_cursor: &mut bool,
    #[cfg(unix)] remote_image_paste_key: &mut Option<(
        crossterm::event::KeyCode,
        crossterm::event::KeyModifiers,
    )>,
) {
    match crate::config::load_live_config() {
        Ok(loaded) => {
            for diagnostic in loaded.config.ui.sound.diagnostics() {
                warn!(diagnostic = %diagnostic, "local sound config diagnostic");
            }
            #[cfg(unix)]
            let loaded_remote_image_paste_key = client_remote_image_paste_key(&loaded.config);
            *sound_config = loaded.config.ui.sound;
            *redraw_on_focus_gained = loaded.config.ui.redraw_on_focus_gained;
            *draw_host_cursor = should_draw_host_cursor(loaded.config.ui.host_cursor);
            #[cfg(unix)]
            {
                *remote_image_paste_key = loaded_remote_image_paste_key;
            }
            debug!("reloaded local client config");
        }
        Err(diagnostics) => {
            warn!(diagnostics = ?diagnostics, "failed to reload local client config; keeping current client config");
        }
    }
}

fn handle_notify(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
) {
    handle_notify_with_notifiers(
        kind,
        message,
        body,
        sound_config,
        crate::terminal_notify::show_notification,
        crate::platform::show_desktop_notification,
    );
}

fn handle_notify_with_notifiers(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
    mut show_terminal_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
    mut show_system_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
) {
    match kind {
        NotifyKind::Sound => {
            let Some(sound) = sound_from_notify_message(message) else {
                warn!(
                    message = message,
                    "received unknown sound notification from server"
                );
                return;
            };
            if sound_config.enabled {
                crate::sound::play(sound, sound_config);
            }
        }
        NotifyKind::Toast => {
            debug!(
                message = message,
                "received terminal toast notification from server"
            );
            if let Err(err) = show_terminal_notification(message, body) {
                warn!(err = %err, "failed to emit terminal notification");
            }
        }
        NotifyKind::SystemToast => {
            debug!(
                message = message,
                "received system toast notification from server"
            );
            if let Err(err) = show_system_notification(message, body) {
                warn!(err = %err, "failed to emit system notification");
            }
        }
    }
}

fn sound_from_notify_message(message: &str) -> Option<crate::sound::Sound> {
    match message {
        "agent done" => Some(crate::sound::Sound::Done),
        "agent attention" => Some(crate::sound::Sound::Request),
        _ => None,
    }
}

#[cfg(unix)]
fn should_bridge_clipboard_image_paste(
    data: &[u8],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if data == b"\x1b[200~\x1b[201~" {
        return is_remote_client;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };

    let events = crate::raw_input::parse_raw_input_bytes_sync(data);
    matches!(
        events.as_slice(),
        [crate::raw_input::RawInputEvent::Key(key)]
            if key.kind == crossterm::event::KeyEventKind::Press
                && crate::config::terminal_key_matches_combo(*key, remote_image_paste_key)
    )
}

#[cfg(unix)]
fn read_image_file_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let (path, extension) = image_path_from_terminal_drop(data, is_remote_client)?;
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let file = std::fs::File::open(&path).ok()?;
    let bytes =
        match crate::platform::read_limited_reader(file, MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()? {
            crate::platform::LimitedRead::Complete(bytes) => bytes,
            crate::platform::LimitedRead::Empty => return None,
            crate::platform::LimitedRead::Oversized => {
                warn!(
                    max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                    "local image file drop is too large to bridge"
                );
                return None;
            }
        };

    Some(crate::platform::ClipboardImage { bytes, extension })
}

#[cfg(unix)]
fn image_path_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<(std::path::PathBuf, &'static str)> {
    if !is_remote_client {
        return None;
    }

    let bytes = bracketed_paste_payload(data).unwrap_or(data);
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.is_empty() || text.contains(['\r', '\n']) {
        return None;
    }

    let text = unescape_terminal_drop_path(strip_matching_path_quotes(text));
    let path = std::path::PathBuf::from(text);
    if !path.is_absolute() {
        return None;
    }

    let extension = recognized_image_extension(path.extension()?.to_str()?)?;
    Some((path, extension))
}

#[cfg(unix)]
fn bracketed_paste_payload(data: &[u8]) -> Option<&[u8]> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    data.strip_prefix(START)?.strip_suffix(END)
}

#[cfg(unix)]
fn strip_matching_path_quotes(text: &str) -> &str {
    if text.len() < 2 {
        return text;
    }

    let bytes = text.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) => &text[1..text.len() - 1],
        _ => text,
    }
}

#[cfg(unix)]
fn unescape_terminal_drop_path(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                unescaped.push(escaped);
            } else {
                unescaped.push(ch);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
}

#[cfg(unix)]
fn recognized_image_extension(extension: &str) -> Option<&'static str> {
    if extension.eq_ignore_ascii_case("png") {
        Some("png")
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        Some("jpg")
    } else if extension.eq_ignore_ascii_case("gif") {
        Some("gif")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some("bmp")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Clipboard forwarding
// ---------------------------------------------------------------------------

/// Decode a clipboard payload forwarded by the server.
fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn forwarded_clipboard_osc52(data: &str) -> Option<Vec<u8>> {
    let bytes = decode_clipboard_payload(data)?;
    Some(crate::selection::osc52_sequence(&bytes).into_bytes())
}

/// Forwards a clipboard write from the server to the local client clipboard.
fn forward_clipboard(data: &str) {
    let Some(sequence) = forwarded_clipboard_osc52(data) else {
        warn!("received invalid clipboard payload from server");
        return;
    };

    // The app client owns terminal stdout, so use OSC 52 directly here. Both
    // client and monolithic modes must keep clipboard-owner processes such as
    // wl-copy/xclip out of their input/render loops.
    let mut stdout = io::stdout();
    let _ = stdout.write_all(&sequence);
    let _ = stdout.flush();
}

fn window_title_osc(title: Option<&str>) -> Vec<u8> {
    let title = title.unwrap_or("herdr");
    let safe_title = title
        .chars()
        .filter(|ch| !matches!(*ch, '\u{1b}' | '\u{7}' | '\u{9c}'))
        .collect::<String>();
    format!("\x1b]0;{safe_title}\x07").into_bytes()
}

fn write_window_title(title: Option<&str>) {
    let _ = io::stdout().write_all(&window_title_osc(title));
}

// ---------------------------------------------------------------------------
// Frame output
// ---------------------------------------------------------------------------

fn write_encoded_frame_with_graphics(
    mut writer: impl io::Write,
    encoded: &[u8],
    graphics: &[u8],
) -> io::Result<()> {
    writer.write_all(encoded)?;
    if graphics.is_empty() {
        return Ok(());
    }

    record_received_kitty_graphics(graphics);
    writer.write_all(b"\x1b7")?;
    writer.write_all(graphics)?;
    writer.write_all(b"\x1b8")
}

fn contains_kitty_graphics_bytes(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| window == b"\x1b_G")
}

fn record_received_kitty_graphics(bytes: &[u8]) {
    let ids = kitty_graphics_image_ids(bytes);
    if ids.is_empty() {
        return;
    }
    let set = RECEIVED_KITTY_GRAPHICS_IDS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut set) = set.lock() {
        set.extend(ids);
    }
}

fn clear_received_kitty_graphics(mut writer: impl io::Write) -> io::Result<()> {
    let Some(set) = RECEIVED_KITTY_GRAPHICS_IDS.get() else {
        return Ok(());
    };
    let Ok(mut set) = set.lock() else {
        return Ok(());
    };
    for id in set.drain() {
        write!(writer, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
    }
    writer.flush()
}

fn kitty_graphics_image_ids(bytes: &[u8]) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut index = 0usize;
    while let Some(start) = find_subslice(&bytes[index..], b"\x1b_G") {
        let command_start = index + start + 3;
        let Some(end) = find_subslice(&bytes[command_start..], b"\x1b\\") else {
            break;
        };
        let command = &bytes[command_start..command_start + end];
        if let Some(id) = kitty_graphics_command_image_id(command) {
            ids.push(id);
        }
        index = command_start + end + 2;
    }
    ids
}

fn kitty_graphics_command_image_id(command: &[u8]) -> Option<u32> {
    let header_end = command
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(command.len());
    for part in command[..header_end].split(|byte| *byte == b',') {
        let Some(value) = part.strip_prefix(b"i=") else {
            continue;
        };
        let text = std::str::from_utf8(value).ok()?;
        if let Ok(id) = text.parse::<u32>() {
            return Some(id);
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Resize polling
// ---------------------------------------------------------------------------

fn current_terminal_geometry(kitty_graphics_enabled: bool) -> (u16, u16, u32, u32) {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if !kitty_graphics_enabled {
        return (cols, rows, 0, 0);
    }
    let Ok(size) = crossterm::terminal::window_size() else {
        return (cols, rows, 8, 16);
    };
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return (cols, rows, 8, 16);
    }
    (
        cols,
        rows,
        (size.width as u32 / size.columns as u32).max(1),
        (size.height as u32 / size.rows as u32).max(1),
    )
}

/// Polls the terminal size and sends resize events when it changes.
fn resize_poll_loop(
    resize_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    initial_cols: u16,
    initial_rows: u16,
    kitty_graphics_enabled: bool,
    should_quit: &Arc<AtomicBool>,
) {
    let (_, _, initial_cell_width, initial_cell_height) =
        current_terminal_geometry(kitty_graphics_enabled);
    let mut last_size = (
        initial_cols,
        initial_rows,
        initial_cell_width,
        initial_cell_height,
    );
    while !should_quit.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let new_size = current_terminal_geometry(kitty_graphics_enabled);
        if new_size != last_size {
            last_size = new_size;
            if resize_tx
                .blocking_send(ClientLoopEvent::Resize(
                    new_size.0, new_size.1, new_size.2, new_size.3,
                ))
                .is_err()
            {
                break; // Main loop gone.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Initialize logging for the client process.
fn query_host_terminal_theme() {
    let _ = write_host_terminal_theme_query(io::stdout());
}

fn should_query_host_terminal_theme() -> bool {
    !cfg!(windows)
}

fn write_host_terminal_theme_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes())?;
    writer.flush()
}

fn init_logging() {
    crate::logging::init_file_logging("herdr-client.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn restore_env_var(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            restore_env_var(self.key, self.previous.clone());
        }
    }

    #[test]
    fn windows_virtual_terminal_input_mode_sets_only_vti_bit() {
        assert_eq!(windows_virtual_terminal_input_mode(0x01f0), 0x03f0);
        assert_eq!(windows_virtual_terminal_input_mode(0x03f0), 0x03f0);
    }

    struct EnvVarsRemovedGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvVarsRemovedGuard {
        fn new(keys: &[&'static str]) -> Self {
            let previous: Vec<_> = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { previous }
        }
    }

    impl Drop for EnvVarsRemovedGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.clone() {
                restore_env_var(key, value);
            }
        }
    }

    #[test]
    fn host_cursor_policy_auto_uses_platform_default() {
        assert_eq!(
            should_draw_host_cursor(crate::config::HostCursorModeConfig::Auto),
            crate::platform::should_draw_host_cursor_by_default()
        );
    }

    #[test]
    fn host_cursor_policy_native_and_drawn_override_auto_detection() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarGuard::set("TERM_PROGRAM", "WezTerm");

        assert!(!should_draw_host_cursor(
            crate::config::HostCursorModeConfig::Native
        ));
        assert!(should_draw_host_cursor(
            crate::config::HostCursorModeConfig::Drawn
        ));
    }

    #[cfg(unix)]
    #[test]
    fn clipboard_image_paste_bridge_triggers_on_configured_key_and_empty_paste() {
        let ctrl_v = crate::config::parse_key_combo("ctrl+v").unwrap();
        assert!(should_bridge_clipboard_image_paste(
            &[0x16],
            true,
            Some(ctrl_v)
        ));
        assert!(should_bridge_clipboard_image_paste(
            b"\x1b[118;5u",
            true,
            Some(ctrl_v)
        ));
        assert!(should_bridge_clipboard_image_paste(
            b"\x1b[200~\x1b[201~",
            true,
            None
        ));
        assert!(!should_bridge_clipboard_image_paste(
            b"\x1b[200~\x1b[201~",
            false,
            Some(ctrl_v)
        ));
        assert!(!should_bridge_clipboard_image_paste(
            b"\x1b[200~text\x1b[201~",
            true,
            Some(ctrl_v)
        ));
        assert!(!should_bridge_clipboard_image_paste(&[0x16], true, None));
        assert!(!should_bridge_clipboard_image_paste(
            b"v",
            true,
            Some(ctrl_v)
        ));
    }

    #[cfg(unix)]
    struct TempImageFile {
        path: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl TempImageFile {
        fn new(extension: &str, bytes: &[u8]) -> Self {
            Self::with_name_fragment("test", extension, bytes)
        }

        fn with_name_fragment(name_fragment: &str, extension: &str, bytes: &[u8]) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "herdr-client-drop-{name_fragment}-{}-{nanos}.{extension}",
                std::process::id()
            ));
            std::fs::write(&path, bytes).unwrap();
            Self { path }
        }
    }

    #[cfg(unix)]
    impl Drop for TempImageFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_reads_bracketed_absolute_image_path() {
        let file = TempImageFile::new("PNG", b"image-bytes");
        let input = format!("\x1b[200~{}\x1b[201~", file.path.display());

        let image = read_image_file_from_terminal_drop(input.as_bytes(), true).unwrap();

        assert_eq!(image.extension, "png");
        assert_eq!(image.bytes, b"image-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_reads_plain_quoted_path_with_newline() {
        let file = TempImageFile::new("jpeg", b"jpeg-bytes");
        let input = format!("'{}'\n", file.path.display());

        let image = read_image_file_from_terminal_drop(input.as_bytes(), true).unwrap();

        assert_eq!(image.extension, "jpg");
        assert_eq!(image.bytes, b"jpeg-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_unescapes_spaces_in_paths() {
        let file = TempImageFile::with_name_fragment("space test", "png", b"image-bytes");
        let escaped_path = file.path.display().to_string().replace(' ', "\\ ");

        let image = read_image_file_from_terminal_drop(escaped_path.as_bytes(), true).unwrap();

        assert_eq!(image.extension, "png");
        assert_eq!(image.bytes, b"image-bytes");
    }

    #[cfg(unix)]
    #[test]
    fn remote_image_file_drop_bridge_ignores_non_remote_and_non_image_input() {
        let file = TempImageFile::new("png", b"image-bytes");
        let path = file.path.display().to_string();

        assert!(read_image_file_from_terminal_drop(path.as_bytes(), false).is_none());
        assert!(read_image_file_from_terminal_drop(b"relative.png\n", true).is_none());
        assert!(read_image_file_from_terminal_drop(b"/tmp/file.txt\n", true).is_none());
        assert!(read_image_file_from_terminal_drop(
            format!("{}\nextra", file.path.display()).as_bytes(),
            true
        )
        .is_none());
    }

    #[test]
    fn graphics_bytes_are_written_after_blit_with_saved_cursor() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(
            &mut output,
            b"\x1b[?2026htext\x1b[?2026lcursor",
            b"graphics",
        )
        .unwrap();

        assert_eq!(
            output,
            b"\x1b[?2026htext\x1b[?2026lcursor\x1b7graphics\x1b8"
        );
    }

    #[test]
    fn empty_graphics_writes_only_blit_frame() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(&mut output, b"text", b"").unwrap();

        assert_eq!(output, b"text");
    }

    #[test]
    fn terminal_frame_kitty_detection_matches_apc_prefix() {
        assert!(contains_kitty_graphics_bytes(b"text\x1b_Ga=p;\x1b\\"));
        assert!(!contains_kitty_graphics_bytes(b"text\x1b[?2026h"));
    }

    #[test]
    fn kitty_graphics_image_id_parser_tracks_herdr_ids_only() {
        let ids = kitty_graphics_image_ids(
            b"text\x1b_Ga=t,t=d,f=32,s=1,v=1,i=10023,q=2;AAAA\x1b\\\x1b_Ga=p,i=10023,p=7;\x1b\\",
        );
        assert_eq!(ids, vec![10023, 10023]);
    }

    #[test]
    fn kitty_graphics_cleanup_deletes_tracked_images_not_all_images() {
        record_received_kitty_graphics(b"\x1b_Ga=t,i=123,q=2;AAAA\x1b\\");
        let mut output = Vec::new();
        clear_received_kitty_graphics(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("a=d,d=I,i=123"));
        assert!(!text.contains("d=A"));
    }

    #[test]
    fn write_host_terminal_theme_query_emits_osc_queries() {
        let mut output = Vec::new();
        write_host_terminal_theme_query(&mut output).unwrap();
        assert_eq!(
            output,
            crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes()
        );
    }

    #[test]
    fn write_host_color_scheme_report_mode_emits_mode_sequences() {
        let mut output = Vec::new();
        write_host_color_scheme_report_mode(&mut output, true).unwrap();
        write_host_color_scheme_report_mode(&mut output, false).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE.as_bytes(),
        );
        expected.extend_from_slice(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        );
        assert_eq!(output, expected);
    }

    #[test]
    fn color_scheme_change_event_requests_host_theme_query() {
        let events = crate::raw_input::parse_raw_input_bytes_sync(b"\x1b[?997;1n");

        assert!(crate::raw_input::events_require_host_terminal_theme_query(
            &events
        ));
    }

    #[test]
    fn host_terminal_theme_query_is_disabled_on_windows() {
        assert_eq!(should_query_host_terminal_theme(), !cfg!(windows));
    }

    #[test]
    fn color_scheme_reports_are_enabled_only_for_full_clients() {
        assert_eq!(
            should_enable_host_color_scheme_reports(true),
            !cfg!(windows)
        );
        assert!(!should_enable_host_color_scheme_reports(false));
    }

    #[test]
    fn terminal_restore_postlude_restores_visible_default_cursor() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output, false).unwrap();
        assert_eq!(output, b"\x1b[?25h\x1b[0 q");
    }

    #[test]
    fn terminal_restore_postlude_disables_color_scheme_reports_when_enabled() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output, true).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        );
        expected.extend_from_slice(b"\x1b[?25h\x1b[0 q");
        assert_eq!(output, expected);
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_detaches_on_prefix_q() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        assert!(matches!(
            escape.filter_input(vec![b'q'], 24, 3),
            AttachInputAction::Detach
        ));
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_sends_literal_prefix_on_double_prefix() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        match escape.filter_input(vec![0x02], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02]),
            other => panic!("expected forwarded prefix, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_forwards_prefix_before_non_escape_key() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![b'a', 0x02], 24, 3),
            AttachInputAction::Forward(bytes) if bytes == b"a"
        ));
        match escape.filter_input(vec![b'x'], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02, b'x']),
            other => panic!("expected forwarded bytes, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_turns_wheel_into_scroll_action() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[<64;11;6M".to_vec(), 24, 7) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                column,
                row,
                ..
            } => {
                assert_eq!(source, AttachScrollSource::Wheel);
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 7);
                assert_eq!(column, Some(10));
                assert_eq!(row, Some(5));
            }
            other => panic!("expected scroll action, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_swallows_non_wheel_mouse_reports() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(b"\x1b[<0;11;6M".to_vec(), 24, 7),
            AttachInputAction::None
        ));
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_turns_plain_page_keys_into_scroll_actions() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[5~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-up scroll action, got {other:?}"),
        }

        match escape.filter_input(b"\x1b[6~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[6~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Down);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-down scroll action, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn attach_escape_forwards_modified_page_key() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5;5~".to_vec(), 12, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, b"\x1b[5;5~"),
            other => panic!("expected modified page key to forward, got {other:?}"),
        }
    }

    #[test]
    fn client_error_display_connection_failed() {
        let err = ClientError::ConnectionFailed(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("failed to connect to server"),
            "should mention connection failure: {msg}"
        );
        assert!(
            msg.contains("herdr server"),
            "should suggest starting server: {msg}"
        );
    }

    #[test]
    fn client_error_display_handshake_rejected() {
        let err = ClientError::HandshakeRejected {
            version: 1,
            error: "incompatible".into(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("rejected handshake"),
            "should mention rejection: {msg}"
        );
        assert!(msg.contains("incompatible"), "should include error: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown() {
        let err = ClientError::ServerShutdown {
            reason: Some("maintenance".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
        assert!(msg.contains("maintenance"), "should include reason: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown_no_reason() {
        let err = ClientError::ServerShutdown { reason: None };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_default_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarsRemovedGuard::new(&[
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            crate::session::SESSION_ENV_VAR,
        ]);
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr` to reattach"),
            "should suggest default reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_named_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr session attach work` to reattach"),
            "should suggest named session reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_remote_reattach_hint_takes_precedence() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            "herdr --remote host --session work",
        );
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr --remote host --session work` to reattach"),
            "should prefer remote reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_connection_lost() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
        let err =
            ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let msg = err.to_string();
        assert!(
            msg.contains("lost connection to server"),
            "should mention lost connection: {msg}"
        );
    }

    #[test]
    fn client_error_display_remote_connection_lost_has_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            "herdr --remote host --session work",
        );
        let err =
            ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let msg = err.to_string();
        assert!(
            msg.contains("lost connection to remote Herdr"),
            "should mention remote connection loss: {msg}"
        );
        assert!(
            msg.contains("panes may still be running"),
            "should explain possible persistence: {msg}"
        );
        assert!(
            msg.contains("Run `herdr --remote host --session work` to reattach"),
            "should show remote reattach command: {msg}"
        );
    }

    #[test]
    fn sound_from_notify_message_maps_done() {
        assert_eq!(
            sound_from_notify_message("agent done"),
            Some(crate::sound::Sound::Done)
        );
    }

    #[test]
    fn sound_from_notify_message_maps_attention() {
        assert_eq!(
            sound_from_notify_message("agent attention"),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn sound_from_notify_message_rejects_unknown_payloads() {
        assert_eq!(sound_from_notify_message("toast"), None);
    }

    #[test]
    fn reload_local_client_config_refreshes_local_client_presentation_state() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-client-config-reload-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "[ui]\nredraw_on_focus_gained = false\nhost_cursor = \"drawn\"\n",
        )
        .unwrap();
        let path_string = path.to_string_lossy().to_string();
        let _env = EnvVarGuard::set(crate::config::CONFIG_PATH_ENV_VAR, &path_string);
        let mut sound_config = crate::config::SoundConfig::default();
        let mut redraw_on_focus_gained = true;
        let mut draw_host_cursor = false;
        #[cfg(unix)]
        let mut remote_image_paste_key = None;

        reload_local_client_config(
            &mut sound_config,
            &mut redraw_on_focus_gained,
            &mut draw_host_cursor,
            #[cfg(unix)]
            &mut remote_image_paste_key,
        );

        assert!(!redraw_on_focus_gained);
        assert!(draw_host_cursor);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn toast_notify_from_server_is_emitted_even_when_attach_config_was_off() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::Toast,
            "pi finished",
            Some("workspace 1"),
            &sound_config,
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
            |_, _| Ok(false),
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn system_toast_notify_from_server_uses_system_notifier() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::SystemToast,
            "pi finished",
            Some("workspace 1"),
            &sound_config,
            |_, _| Ok(false),
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn system_toast_notify_preserves_colon_in_title() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::SystemToast,
            "build: failed",
            Some("api workspace"),
            &sound_config,
            |_, _| Ok(false),
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
        );

        assert_eq!(
            emitted,
            Some((
                "build: failed".to_string(),
                Some("api workspace".to_string())
            ))
        );
    }

    #[test]
    fn decode_clipboard_payload_decodes_base64() {
        assert_eq!(decode_clipboard_payload("dGVzdA=="), Some(b"test".to_vec()));
    }

    #[test]
    fn decode_clipboard_payload_rejects_invalid_base64() {
        assert_eq!(decode_clipboard_payload("not-base64!!!"), None);
    }

    #[test]
    fn terminal_control_input_command_accepts_text() {
        let action =
            terminal_control_command_from_json(r#"{"type":"terminal.input","text":"hello"}"#)
                .unwrap();
        let ClientMessage::Input { data } = action else {
            panic!("expected input command");
        };
        assert_eq!(data, b"hello");
    }

    #[test]
    fn terminal_control_input_command_accepts_base64_bytes() {
        let action =
            terminal_control_command_from_json(r#"{"type":"terminal.input","bytes":"G1tB"}"#)
                .unwrap();
        let ClientMessage::Input { data } = action else {
            panic!("expected input command");
        };
        assert_eq!(data, b"\x1b[A");
    }

    #[test]
    fn terminal_control_resize_command_maps_to_client_resize() {
        let action = terminal_control_command_from_json(
            r#"{"type":"terminal.resize","cols":100,"rows":30,"cell_width_px":8,"cell_height_px":16}"#,
        )
        .unwrap();
        let ClientMessage::Resize {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        } = action
        else {
            panic!("expected resize command");
        };
        assert_eq!(
            (cols, rows, cell_width_px, cell_height_px),
            (100, 30, 8, 16)
        );
    }

    #[test]
    fn terminal_control_scroll_command_maps_to_attach_scroll() {
        let action = terminal_control_command_from_json(
            r#"{"type":"terminal.scroll","direction":"up","lines":3}"#,
        )
        .unwrap();
        let ClientMessage::AttachScroll {
            source,
            direction,
            lines,
            ..
        } = action
        else {
            panic!("expected scroll command");
        };
        assert_eq!(source, AttachScrollSource::Wheel);
        assert_eq!(direction, AttachScrollDirection::Up);
        assert_eq!(lines, 3);
    }

    #[test]
    fn forwarded_clipboard_uses_direct_osc52_sequence() {
        assert_eq!(
            forwarded_clipboard_osc52("dGVzdA==").as_deref(),
            Some(b"\x1b]52;c;dGVzdA==\x07".as_slice())
        );
        assert_eq!(forwarded_clipboard_osc52("not base64"), None);
    }

    #[test]
    fn window_title_osc_strips_terminators_and_defaults_to_herdr() {
        assert_eq!(
            window_title_osc(Some("herdr\x1b api\u{7}\u{9c}")),
            b"\x1b]0;herdr api\x07"
        );
        assert_eq!(window_title_osc(None), b"\x1b]0;herdr\x07");
    }

    #[cfg(unix)]
    #[test]
    fn authoritative_directory_update_revokes_removed_members() {
        let endpoint = |id: &str| {
            crate::federation::EndpointState::configured(crate::config::FederationEndpointConfig {
                id: id.into(),
                target: id.into(),
                ..crate::config::FederationEndpointConfig::default()
            })
        };
        let mut directory = vec![endpoint("x1"), endpoint("stl-agents-1"), endpoint("tana")];
        merge_federation_directory(
            &mut directory,
            vec![endpoint("x1"), endpoint("stl-agents-1")],
        );
        assert_eq!(
            directory
                .iter()
                .map(|state| state.endpoint.id.as_str())
                .collect::<Vec<_>>(),
            vec!["stl-agents-1", "x1"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn first_federation_dial_shows_connecting_until_activation_finishes() {
        let mut connecting =
            FederationConnectingUi::for_plan(FederationConnectionPlan::New, "STL workbench");

        assert_eq!(
            connecting.as_ref().map(FederationConnectingUi::message),
            Some("Connecting to STL workbench\u{2026}".into())
        );
        assert!(FederationConnectingUi::finish(&mut connecting));
        assert!(connecting.is_none(), "ACK or failure must clear the state");
        assert!(!FederationConnectingUi::finish(&mut connecting));
    }

    #[cfg(unix)]
    #[test]
    fn retained_federation_connection_skips_connecting_ui() {
        assert!(FederationConnectingUi::for_plan(
            FederationConnectionPlan::Suspended(0),
            "STL workbench"
        )
        .is_none());
        assert!(FederationConnectingUi::for_plan(
            FederationConnectionPlan::Current,
            "STL workbench"
        )
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn connecting_overlay_preserves_the_source_frame_outside_its_panel() {
        let base_cell = crate::protocol::CellData {
            symbol: "s".into(),
            fg: crate::protocol::color_to_u32(ratatui::style::Color::Green),
            bg: crate::protocol::color_to_u32(ratatui::style::Color::Black),
            modifier: 0,
            skip: false,
            hyperlink: None,
        };
        let base = crate::protocol::FrameData {
            cells: vec![base_cell; 60 * 11],
            width: 60,
            height: 11,
            cursor: Some(crate::protocol::CursorState {
                x: 2,
                y: 3,
                visible: true,
                shape: 2,
            }),
            hyperlinks: Vec::new(),
            graphics: b"source-graphics".to_vec(),
        };
        let connecting =
            FederationConnectingUi::for_plan(FederationConnectionPlan::New, "STL workbench")
                .expect("cold connection should have UI");

        let overlay = federation_connecting_frame(
            &base,
            &connecting,
            &crate::app::state::Palette::catppuccin(),
        );

        assert_eq!(base.cells[0].symbol, "s");
        assert_eq!(overlay.cells[0], base.cells[0]);
        assert_eq!(overlay.cells[overlay.cells.len() - 1], base.cells[0]);
        assert_eq!(
            overlay.cursor.as_ref().map(|cursor| cursor.visible),
            Some(false)
        );
        assert!(overlay.graphics.is_empty());
        let rendered = overlay
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(rendered.contains("Connecting to STL workbench\u{2026}"));
    }

    #[cfg(unix)]
    #[test]
    fn live_handoff_activation_requires_runtime_identity_continuity() {
        let expected = ("server-stl".to_string(), "session-stl".to_string());
        assert!(federation_activation_identity_matches(
            "stl-agents-1",
            None,
            Some(&expected),
            "stl-agents-1",
            "server-stl",
            "session-stl",
        ));
        assert!(!federation_activation_identity_matches(
            "stl-agents-1",
            None,
            Some(&expected),
            "stl-agents-1",
            "replacement-server",
            "session-stl",
        ));
        assert!(!federation_activation_identity_matches(
            "stl-agents-1",
            None,
            Some(&expected),
            "stl-agents-1",
            "server-stl",
            "replacement-session",
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resource_less_activation_uses_home_snapshot_runtime_identity() {
        let mut endpoint =
            crate::federation::EndpointState::configured(crate::config::FederationEndpointConfig {
                id: "stl-agents-1".into(),
                target: "paul@stl-agents-1".into(),
                ..crate::config::FederationEndpointConfig::default()
            });
        endpoint.snapshot = Some(crate::api::schema::SessionSnapshot {
            identity: crate::api::schema::RuntimeIdentity {
                server_id: "server-stl".into(),
                session_id: "session-stl".into(),
                member_id: "stl-agents-1".into(),
                ..crate::api::schema::RuntimeIdentity::default()
            },
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            event_cursor: 9,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents: Vec::new(),
        });

        assert_eq!(
            federated_endpoint_runtime_identity(&endpoint),
            Some(("server-stl".into(), "session-stl".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn home_directory_authority_reconnect_requires_full_identity_continuity() {
        assert!(directory_authority_identity_matches(
            "x1",
            Some("server-x1"),
            Some("session-x1"),
            "x1",
            "x1",
            "server-x1",
            "session-x1",
        ));
        assert!(!directory_authority_identity_matches(
            "x1",
            Some("replacement-server"),
            Some("session-x1"),
            "x1",
            "x1",
            "server-x1",
            "session-x1",
        ));
        assert!(!directory_authority_identity_matches(
            "x1",
            Some("server-x1"),
            Some("session-x1"),
            "x1",
            "hostile",
            "server-x1",
            "session-x1",
        ));
    }
}
