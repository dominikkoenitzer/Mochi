//! `mochic mcp`: a local MCP server on stdin and stdout, so an AI assistant
//! can read the desktop Mochi manages and arrange it.
//!
//! Every tool goes through the same pipe the rest of `mochic` uses and maps to
//! commands the daemon already takes. No tool stops Mochi, closes a window or
//! writes a file: `reload_config` asks the daemon to read the files it already
//! has, and `check_config` only reads one.
//!
//! The daemon is reached through [`Daemon`], so the tests can stand a fake in
//! its place.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use mochi_client::{Command, CycleDirection, Layout, QueryTarget, Response};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// What every tool says when there is no daemon to ask.
pub const NOT_RUNNING: &str = "Mochi is not running. Start it from the Start menu, or run \
                               `mochic start` in a terminal, then try again.";

/// What the server tells the client about itself when it connects.
const INSTRUCTIONS: &str = "Mochi tiles the windows of this Windows PC. Call get_state first: \
it lists every monitor, workspace and managed window, which one has the focus, and the layout \
of each workspace. Window ids are the hwnd numbers from get_state. Monitor and workspace \
indexes start at 0. A tool that changes the desktop acts on what is on screen at that moment, \
so read the state again after a change. No tool stops Mochi, closes a window or writes a file.";

/// The daemon, as the server reaches it.
pub trait Daemon: Send + Sync {
    /// Sends one command and returns the daemon's answer.
    fn send(&self, command: &Command) -> mochi_client::Result<Response>;
}

/// The running daemon, over its named pipe.
pub struct Pipe;

impl Daemon for Pipe {
    fn send(&self, command: &Command) -> mochi_client::Result<Response> {
        mochi_client::send(command)
    }
}

/// Checks a configuration file the way `mochic check --json` does and answers
/// with its document, or with the message of the error document.
pub type Checker = dyn Fn(Option<PathBuf>) -> std::result::Result<Value, String> + Send + Sync;

/// The real checker: the code behind `mochic check --json`.
fn check_config_file(path: Option<PathBuf>) -> std::result::Result<Value, String> {
    match crate::check_document(path.as_deref()) {
        Ok(out) => serde_json::to_value(out).map_err(|e| e.to_string()),
        Err(failure) => Err(failure.error.message),
    }
}

/// A tool's failure, in words the assistant can pass on.
type Outcome = std::result::Result<Value, String>;

// Talking to the daemon
// ---------------------

/// Sends one command, turning every way it can fail into one sentence.
fn ask(daemon: &dyn Daemon, command: &Command) -> std::result::Result<Response, String> {
    match daemon.send(command) {
        Ok(Response::Error { message }) | Err(mochi_client::Error::Daemon(message)) => {
            Err(format!("Mochi refused `{}`: {message}", command.name()))
        }
        Ok(response) => Ok(response),
        Err(mochi_client::Error::NotRunning) => Err(NOT_RUNNING.to_owned()),
        Err(e) => Err(format!("could not talk to Mochi: {e}")),
    }
}

/// Says that the daemon answered a command with the wrong kind of response.
fn wrong_answer(command: &Command, response: &Response) -> String {
    let given = serde_json::to_value(response)
        .ok()
        .and_then(|v| v["response"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "something else".to_owned());
    format!(
        "Mochi answered `{given}` to `{}`, which this tool cannot use",
        command.name()
    )
}

/// The daemon's state document.
fn read_state(daemon: &dyn Daemon) -> Outcome {
    let command = Command::State;
    match ask(daemon, &command)? {
        Response::State { state } => Ok(state),
        other => Err(wrong_answer(&command, &other)),
    }
}

/// The commands one tool call sent, in order, so the result can say what was
/// done and a failure half way can say how far it got.
#[derive(Default)]
struct Sent(Vec<&'static str>);

impl Sent {
    /// Sends a command that is answered with a plain `ok`.
    fn act(&mut self, daemon: &dyn Daemon, command: Command) -> std::result::Result<(), String> {
        match ask(daemon, &command) {
            Ok(Response::Ok) => {
                self.0.push(command.name());
                Ok(())
            }
            Ok(other) => Err(self.after(wrong_answer(&command, &other))),
            Err(e) => Err(self.after(e)),
        }
    }

    /// A failure, with what was already done before it.
    fn after(&self, message: String) -> String {
        if self.0.is_empty() {
            message
        } else {
            format!("{message} (already sent: {})", self.0.join(", "))
        }
    }

    fn list(&self) -> Value {
        json!(self.0)
    }
}

// Finding a window in the state document
// --------------------------------------

/// Where a managed window sits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    monitor: usize,
    workspace: usize,
    spot: Spot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Spot {
    /// In the tiled ring: container `container`, at `position` in its stack.
    Tiled {
        container: usize,
        position: usize,
        stacked: bool,
    },
    Monocle,
    Maximized,
    Floating,
}

fn is_window(window: &Value, hwnd: u64) -> bool {
    window["hwnd"].as_u64() == Some(hwnd)
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// Finds a managed window, and the object the state document has for it.
fn locate(state: &Value, hwnd: u64) -> Option<(Place, &Value)> {
    for (monitor, m) in array(&state["monitors"]).iter().enumerate() {
        for (workspace, w) in array(&m["workspaces"]).iter().enumerate() {
            let place = |spot| Place {
                monitor,
                workspace,
                spot,
            };
            for (container, c) in array(&w["containers"]).iter().enumerate() {
                let windows = array(&c["windows"]);
                if let Some(position) = windows.iter().position(|x| is_window(x, hwnd)) {
                    let spot = Spot::Tiled {
                        container,
                        position,
                        stacked: windows.len() > 1,
                    };
                    return Some((place(spot), &windows[position]));
                }
            }
            if let Some(window) = array(&w["monocle_container"]["windows"])
                .iter()
                .find(|x| is_window(x, hwnd))
            {
                return Some((place(Spot::Monocle), window));
            }
            if is_window(&w["maximized_window"], hwnd) {
                return Some((place(Spot::Maximized), &w["maximized_window"]));
            }
            if let Some(window) = array(&w["floating_windows"])
                .iter()
                .find(|x| is_window(x, hwnd))
            {
                return Some((place(Spot::Floating), window));
            }
        }
    }
    None
}

fn not_managed(hwnd: u64) -> String {
    format!(
        "Mochi does not manage a window with the id {hwnd}. Window ids are the hwnd numbers \
         get_state lists; call it again, the window may have closed."
    )
}

// The tools
// ---------

fn get_state(daemon: &dyn Daemon) -> Outcome {
    read_state(daemon)
}

fn why_not_tiled(daemon: &dyn Daemon, p: WhyParams) -> Outcome {
    let Some(hwnd) = p.window else {
        return explain_front(daemon);
    };
    let state = read_state(daemon)?;
    if let Some((place, window)) = locate(&state, hwnd) {
        return Ok(json!({
            "hwnd": format!("0x{hwnd:x}"),
            "title": window["title"],
            "exe": window["exe"],
            "class": window["class"],
            "managed": true,
            "monitor": place.monitor,
            "workspace": place.workspace,
        }));
    }
    if state["foreground_window"].as_u64() == Some(hwnd) {
        let why = explain_front(daemon)?;
        // The window in front may have changed between the two questions.
        if why["hwnd"] == json!(format!("0x{hwnd:x}")) {
            return Ok(why);
        }
    }
    Err(format!(
        "Mochi does not manage window {hwnd}, and it can only explain the window in front. Ask \
         the user to bring that window to the front, then call why_not_tiled without a window."
    ))
}

fn explain_front(daemon: &dyn Daemon) -> Outcome {
    let command = Command::Why;
    match ask(daemon, &command)? {
        Response::Why { why } => Ok(why),
        other => Err(wrong_answer(&command, &other)),
    }
}

fn list_hotkeys(daemon: &dyn Daemon) -> Outcome {
    let command = Command::Hotkeys;
    match ask(daemon, &command)? {
        Response::Hotkeys { hotkeys } => Ok(hotkeys),
        other => Err(wrong_answer(&command, &other)),
    }
}

/// Brings a managed window into focus with the commands the daemon has:
/// its monitor, its workspace, then along the ring to its container and to
/// its place in a stack. Checks the result, because a mode in front of the
/// ring (monocle, maximize, a floating focus) leaves the ring commands with
/// nothing to do.
fn focus(daemon: &dyn Daemon, hwnd: u64, sent: &mut Sent) -> std::result::Result<Place, String> {
    let mut state = read_state(daemon)?;
    let (place, _) = locate(&state, hwnd).ok_or_else(|| not_managed(hwnd))?;
    if state["focused_window"].as_u64() == Some(hwnd) {
        return Ok(place);
    }

    let mut switched = false;
    if state["focused_monitor"].as_u64() != Some(place.monitor as u64) {
        sent.act(
            daemon,
            Command::FocusMonitor {
                index: place.monitor,
            },
        )?;
        switched = true;
    }
    // Each monitor keeps the workspace it shows, so after a monitor switch
    // the workspace may already be the right one. Asking for it again would
    // only overwrite the workspace `focus-last-workspace` goes back to.
    if state["monitors"][place.monitor]["focused_workspace"].as_u64()
        != Some(place.workspace as u64)
    {
        sent.act(
            daemon,
            Command::FocusWorkspace {
                index: place.workspace,
            },
        )?;
        switched = true;
    }
    if switched {
        state = read_state(daemon).map_err(|e| sent.after(e))?;
    }

    if let Spot::Tiled {
        container,
        position,
        stacked,
    } = place.spot
    {
        let workspace = &state["monitors"][place.monitor]["workspaces"][place.workspace];
        let count = array(&workspace["containers"]).len();
        let current = workspace["focused_container"]
            .as_u64()
            .map_or(0, |c| c as usize);
        if count > 0 {
            // The ring wraps, so go whichever way is shorter: every step
            // moves the focus on screen.
            let ahead = (container + count - current % count) % count;
            let (direction, steps) = if count - ahead < ahead {
                (CycleDirection::Previous, count - ahead)
            } else {
                (CycleDirection::Next, ahead)
            };
            for _ in 0..steps {
                sent.act(daemon, Command::CycleFocus { direction })?;
            }
        }
        let showing = workspace["containers"][container]["focused_window"].as_u64();
        if stacked && showing != Some(hwnd) {
            sent.act(daemon, Command::FocusStackWindow { index: position })?;
        }
    }

    let after = read_state(daemon).map_err(|e| sent.after(e))?;
    if after["focused_window"].as_u64() == Some(hwnd) {
        return Ok(place);
    }
    let shown = format!("monitor {}, workspace {}", place.monitor, place.workspace);
    Err(sent.after(if place.spot == Spot::Floating {
        format!(
            "window {hwnd} floats on {shown}, which is showing now, but Mochi has no command \
             that focuses one floating window. The user can click it."
        )
    } else {
        format!(
            "Mochi did not move the focus to window {hwnd} on {shown}. A monocle, a maximized \
             window or a floating window may hold the focus there; call get_state to see it."
        )
    }))
}

fn focus_window(daemon: &dyn Daemon, p: &FocusParams) -> Outcome {
    let mut sent = Sent::default();
    let place = focus(daemon, p.window, &mut sent)?;
    Ok(json!({
        "ok": true,
        "window": p.window,
        "monitor": place.monitor,
        "workspace": place.workspace,
        "sent": sent.list(),
    }))
}

/// Focuses `window` when one is given, so the next command acts on it.
fn focus_if_given(
    daemon: &dyn Daemon,
    window: Option<u64>,
    sent: &mut Sent,
) -> std::result::Result<(), String> {
    match window {
        Some(hwnd) => focus(daemon, hwnd, sent).map(|_| ()),
        None => Ok(()),
    }
}

fn move_window_to_workspace(daemon: &dyn Daemon, p: &MoveParams) -> Outcome {
    let command = match (p.workspace, &p.workspace_name, p.follow) {
        (Some(_), Some(_), _) => {
            return Err("give either workspace or workspace_name, not both".to_owned());
        }
        (None, None, _) => return Err("give workspace or workspace_name".to_owned()),
        (Some(index), None, true) => Command::MoveToWorkspace { index },
        (Some(index), None, false) => Command::SendToWorkspace { index },
        (None, Some(name), true) => Command::MoveToNamedWorkspace { name: name.clone() },
        (None, Some(name), false) => Command::SendToNamedWorkspace { name: name.clone() },
    };
    let mut sent = Sent::default();
    focus_if_given(daemon, p.window, &mut sent)?;
    sent.act(daemon, command)?;
    Ok(json!({
        "ok": true,
        "window": p.window,
        "workspace": p.workspace.map_or_else(|| json!(p.workspace_name), |i| json!(i)),
        "followed": p.follow,
        "sent": sent.list(),
    }))
}

fn switch_workspace(daemon: &dyn Daemon, p: &SwitchParams) -> Outcome {
    let mut sent = Sent::default();
    match (p.workspace, &p.name) {
        (Some(_), Some(_)) => return Err("give either workspace or name, not both".to_owned()),
        (None, None) => return Err("give workspace or name".to_owned()),
        (None, Some(name)) => {
            if p.monitor.is_some() {
                return Err(
                    "a workspace name already says which monitor; leave monitor out".to_owned(),
                );
            }
            sent.act(daemon, Command::FocusNamedWorkspace { name: name.clone() })?;
        }
        (Some(index), None) => {
            if let Some(monitor) = p.monitor {
                sent.act(daemon, Command::FocusMonitor { index: monitor })?;
            }
            sent.act(daemon, Command::FocusWorkspace { index })?;
        }
    }
    Ok(json!({ "ok": true, "sent": sent.list() }))
}

fn set_layout(daemon: &dyn Daemon, p: &LayoutParams) -> Outcome {
    let layout = p.layout.wire()?;
    let mut sent = Sent::default();
    sent.act(daemon, Command::ChangeLayout { layout })?;
    Ok(json!({ "ok": true, "layout": p.layout.name(), "sent": sent.list() }))
}

fn toggle(daemon: &dyn Daemon, p: &WindowParams, command: Command) -> Outcome {
    let mut sent = Sent::default();
    focus_if_given(daemon, p.window, &mut sent)?;
    sent.act(daemon, command)?;
    Ok(json!({ "ok": true, "window": p.window, "sent": sent.list() }))
}

fn simple(daemon: &dyn Daemon, command: Command) -> Outcome {
    let mut sent = Sent::default();
    sent.act(daemon, command)?;
    Ok(json!({ "ok": true, "sent": sent.list() }))
}

/// Pauses or resumes, whichever `paused` asks for, and leaves it alone when it
/// already is that way. The daemon only has a toggle.
fn set_paused(daemon: &dyn Daemon, paused: bool) -> Outcome {
    let query = Command::Query {
        target: QueryTarget::Paused,
    };
    let now = match ask(daemon, &query)? {
        Response::Query { answer } => answer
            .as_bool()
            .ok_or_else(|| format!("Mochi answered `{answer}` when asked whether it is paused"))?,
        other => return Err(wrong_answer(&query, &other)),
    };
    let mut sent = Sent::default();
    if now != paused {
        sent.act(daemon, Command::TogglePause)?;
    }
    Ok(json!({
        "ok": true,
        "paused": paused,
        "changed": now != paused,
        "sent": sent.list(),
    }))
}

fn check_config(checker: &Checker, p: CheckParams) -> Outcome {
    checker(p.path.map(PathBuf::from))
}

// Arguments
// ---------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WhyParams {
    /// The window to explain, as an hwnd number from get_state. Leave it out
    /// for the window in front.
    #[serde(default)]
    pub window: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckParams {
    /// The file to check. Leave it out for the file Mochi loads.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FocusParams {
    /// The window to focus, as an hwnd number from get_state.
    pub window: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WindowParams {
    /// The window to act on, as an hwnd number from get_state. Leave it out
    /// for the focused window.
    #[serde(default)]
    pub window: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveParams {
    /// The window to move, as an hwnd number from get_state. Leave it out for
    /// the focused window.
    #[serde(default)]
    pub window: Option<u64>,
    /// The workspace to move it to: its zero-based index on the window's
    /// monitor. Give this or workspace_name.
    #[serde(default)]
    pub workspace: Option<usize>,
    /// The workspace to move it to, by name, on whichever monitor it is.
    /// Give this or workspace.
    #[serde(default)]
    pub workspace_name: Option<String>,
    /// True to show that workspace afterwards and keep the focus on the
    /// window. False, the default, leaves the screen as it is.
    #[serde(default)]
    pub follow: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SwitchParams {
    /// The workspace to show: its zero-based index on the monitor. Give this
    /// or name.
    #[serde(default)]
    pub workspace: Option<usize>,
    /// The zero-based monitor index the workspace is on. Leave it out for the
    /// focused monitor.
    #[serde(default)]
    pub monitor: Option<usize>,
    /// The workspace to show, by name, on whichever monitor it is. Give this
    /// or workspace.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LayoutParams {
    /// The layout to use on the focused workspace.
    pub layout: LayoutName,
}

/// The layouts, spelled the way the command line spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutName {
    Bsp,
    Columns,
    Rows,
    VerticalStack,
    HorizontalStack,
    UltrawideVerticalStack,
    Grid,
}

impl LayoutName {
    #[cfg(test)]
    const ALL: [LayoutName; 7] = [
        LayoutName::Bsp,
        LayoutName::Columns,
        LayoutName::Rows,
        LayoutName::VerticalStack,
        LayoutName::HorizontalStack,
        LayoutName::UltrawideVerticalStack,
        LayoutName::Grid,
    ];

    fn name(self) -> &'static str {
        match self {
            LayoutName::Bsp => "bsp",
            LayoutName::Columns => "columns",
            LayoutName::Rows => "rows",
            LayoutName::VerticalStack => "vertical-stack",
            LayoutName::HorizontalStack => "horizontal-stack",
            LayoutName::UltrawideVerticalStack => "ultrawide-vertical-stack",
            LayoutName::Grid => "grid",
        }
    }

    /// The protocol's layout of the same name.
    fn wire(self) -> std::result::Result<Layout, String> {
        serde_json::from_value(json!(self.name()))
            .map_err(|e| format!("Mochi has no layout `{}`: {e}", self.name()))
    }
}

// The server
// ----------

#[derive(Clone)]
pub struct Server {
    daemon: Arc<dyn Daemon>,
    checker: Arc<Checker>,
    /// Held for the whole of a tool call. A tool that focuses a window and
    /// then acts on it must not have another call move the focus in between.
    one_at_a_time: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    /// Runs a tool off the protocol's thread, one at a time. A failure
    /// becomes a tool error the assistant can read.
    async fn run<F>(&self, work: F) -> CallToolResult
    where
        F: FnOnce(&dyn Daemon, &Checker) -> Outcome + Send + 'static,
    {
        let daemon = Arc::clone(&self.daemon);
        let checker = Arc::clone(&self.checker);
        let lock = Arc::clone(&self.one_at_a_time);
        let done = tokio::task::spawn_blocking(move || {
            let _turn = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            work(&*daemon, &*checker)
        })
        .await;
        match done {
            Ok(Ok(value)) => CallToolResult::structured(value),
            Ok(Err(message)) => CallToolResult::error(vec![ContentBlock::text(message)]),
            Err(e) => {
                CallToolResult::error(vec![ContentBlock::text(format!("the tool stopped: {e}"))])
            }
        }
    }
}

#[tool_router(router = tool_router)]
impl Server {
    pub fn new(daemon: Arc<dyn Daemon>, checker: Arc<Checker>) -> Self {
        Server {
            daemon,
            checker,
            one_at_a_time: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    /// Read what Mochi manages right now: every monitor with its workspaces;
    /// each workspace's name, layout, monocle and tiling flags and its
    /// containers; every managed window with its hwnd (the window id the other
    /// tools take), title, exe and class; which monitor, workspace and window
    /// have the focus; and whether tiling is paused. The same document as
    /// `mochic state --json`. Reads only.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn get_state(&self) -> CallToolResult {
        self.run(|d, _| get_state(d)).await
    }

    /// Explain why a window is or is not tiled: managed with its monitor and
    /// workspace, or the reason Mochi leaves it alone (a rule, a tool window,
    /// a dialog, too small, running as administrator, paused, and so on) and
    /// whether a manage rule could change that. Without a window it explains
    /// the window in front. Mochi can only explain an unmanaged window while
    /// it is in front. Reads only.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn why_not_tiled(&self, Parameters(p): Parameters<WhyParams>) -> CallToolResult {
        self.run(move |d, _| why_not_tiled(d, p)).await
    }

    /// List the hotkeys Mochi has bound: the keys, the command each runs, the
    /// binding mode it belongs to, and any line of the hotkey file that did
    /// not parse. Reads only.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn list_hotkeys(&self) -> CallToolResult {
        self.run(|d, _| list_hotkeys(d)).await
    }

    /// Check a Mochi configuration file and its hotkey file without loading
    /// it: whether Mochi could start with it, what to fix first, every error,
    /// warning and note. Without a path it checks the file Mochi loads. Works
    /// when Mochi is not running. Reads only.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn check_config(&self, Parameters(p): Parameters<CheckParams>) -> CallToolResult {
        self.run(move |_, c| check_config(c, p)).await
    }

    /// Focus a managed window: show its monitor and workspace and move the
    /// focus to it, inside a stack too. Answers with where it is and the
    /// Mochi commands that were sent.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn focus_window(&self, Parameters(p): Parameters<FocusParams>) -> CallToolResult {
        self.run(move |d, _| focus_window(d, &p)).await
    }

    /// Move a window to another workspace, by index on its monitor or by
    /// name. By default the screen stays as it is; with follow the view goes
    /// along with the window. Without a window it moves the focused one.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = false
    ))]
    async fn move_window_to_workspace(
        &self,
        Parameters(p): Parameters<MoveParams>,
    ) -> CallToolResult {
        self.run(move |d, _| move_window_to_workspace(d, &p)).await
    }

    /// Show a workspace, by index (on the focused monitor, or on the monitor
    /// given) or by name.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn switch_workspace(&self, Parameters(p): Parameters<SwitchParams>) -> CallToolResult {
        self.run(move |d, _| switch_workspace(d, &p)).await
    }

    /// Set the layout of the focused workspace. To change another workspace,
    /// switch to it first.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn set_layout(&self, Parameters(p): Parameters<LayoutParams>) -> CallToolResult {
        self.run(move |d, _| set_layout(d, &p)).await
    }

    /// Toggle a window between tiled and floating. Without a window it
    /// toggles the focused one.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = false
    ))]
    async fn toggle_float(&self, Parameters(p): Parameters<WindowParams>) -> CallToolResult {
        self.run(move |d, _| toggle(d, &p, Command::ToggleFloat))
            .await
    }

    /// Toggle monocle for a window: its container fills the workspace, and
    /// toggling again puts the layout back. Without a window it toggles the
    /// focused one.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = false
    ))]
    async fn toggle_monocle(&self, Parameters(p): Parameters<WindowParams>) -> CallToolResult {
        self.run(move |d, _| toggle(d, &p, Command::ToggleMonocle))
            .await
    }

    /// Recompute every layout and put every managed window back in its tile.
    /// The first thing to try when windows are out of place.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn retile(&self) -> CallToolResult {
        self.run(|d, _| simple(d, Command::Retile)).await
    }

    /// Pause tiling: windows stay where they are and Mochi stops arranging
    /// them until resume_tiling. Does nothing when already paused.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn pause_tiling(&self) -> CallToolResult {
        self.run(|d, _| set_paused(d, true)).await
    }

    /// Resume tiling after a pause; Mochi picks up windows opened meanwhile
    /// and retiles. Does nothing when not paused.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn resume_tiling(&self) -> CallToolResult {
        self.run(|d, _| set_paused(d, false)).await
    }

    /// Make Mochi read its configuration file and hotkey file again, after
    /// the user changed them. A file Mochi cannot use is refused with its
    /// reason; check_config shows every problem in it.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = true,
        open_world_hint = false
    ))]
    async fn reload_config(&self) -> CallToolResult {
        self.run(|d, _| simple(d, Command::ReloadConfiguration))
            .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mochi", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// `mochic mcp`: serves the running daemon on stdin and stdout until the
/// client leaves. Stdout carries only the protocol.
pub fn serve() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let server = Server::new(Arc::new(Pipe), Arc::new(check_config_file));
        let running = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| anyhow!("the MCP client did not connect: {e}"))?;
        running.waiting().await?;
        Ok(())
    })
}
