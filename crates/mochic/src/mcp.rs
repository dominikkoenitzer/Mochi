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

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::time::Duration;

    use tokio::io::{
        AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
    };

    /// A daemon that answers from a script and remembers what it was sent.
    #[derive(Default)]
    struct Fake {
        stopped: bool,
        /// Successive answers to `state`; the last one repeats.
        states: Mutex<VecDeque<Value>>,
        paused: bool,
        why: Value,
        /// A command the daemon refuses, by its wire name, and with what.
        refuse: Option<(&'static str, &'static str)>,
        sent: Mutex<Vec<Command>>,
    }

    impl Fake {
        fn with_states(states: impl IntoIterator<Item = Value>) -> Self {
            Fake {
                states: Mutex::new(states.into_iter().collect()),
                why: json!({
                    "hwnd": "0x99",
                    "title": "Overlay",
                    "exe": "overlay.exe",
                    "class": "O",
                    "managed": false,
                    "reason": "tool window",
                    "overridable": true,
                }),
                ..Fake::default()
            }
        }

        fn sent(&self) -> Vec<Command> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl Daemon for Fake {
        fn send(&self, command: &Command) -> mochi_client::Result<Response> {
            if self.stopped {
                return Err(mochi_client::Error::NotRunning);
            }
            self.sent.lock().unwrap().push(command.clone());
            if let Some((name, message)) = self.refuse
                && command.name() == name
            {
                return Err(mochi_client::Error::Daemon(message.to_owned()));
            }
            Ok(match command {
                Command::State => {
                    let mut states = self.states.lock().unwrap();
                    let state = if states.len() > 1 {
                        states.pop_front().unwrap()
                    } else {
                        states.front().cloned().unwrap_or_default()
                    };
                    Response::State { state }
                }
                Command::Query {
                    target: QueryTarget::Paused,
                } => Response::Query {
                    answer: json!(self.paused),
                },
                Command::Why => Response::Why {
                    why: self.why.clone(),
                },
                Command::Hotkeys => Response::Hotkeys {
                    hotkeys: json!({
                        "path": null,
                        "gate": "all",
                        "errors": [],
                        "bindings": [{ "keys": "alt + h", "command": "focus left" }],
                    }),
                },
                _ => Response::Ok,
            })
        }
    }

    fn window(hwnd: u64, title: &str) -> Value {
        json!({ "hwnd": hwnd, "title": title, "exe": format!("{title}.exe"), "class": "C" })
    }

    fn container(windows: &[Value], showing: u64) -> Value {
        json!({ "windows": windows, "focused_window": showing, "stack": windows.len() > 1 })
    }

    /// Two monitors. Monitor 0 has workspace 0 with three containers, the
    /// third a stack of 0x30 and 0x31, and workspace 1 with a floating 0x40.
    /// Monitor 1 shows workspace 0 with 0x50 in monocle.
    fn desktop(focused_monitor: u64, shown: u64, focused_container: u64, focused: u64) -> Value {
        json!({
            "paused": false,
            "focused_monitor": focused_monitor,
            "focused_window": focused,
            "foreground_window": 0x99,
            "monitors": [
                {
                    "focused_workspace": shown,
                    "workspaces": [
                        {
                            "name": "1",
                            "layout": "BSP",
                            "focused_container": focused_container,
                            "containers": [
                                container(&[window(0x10, "editor")], 0x10),
                                container(&[window(0x20, "browser")], 0x20),
                                container(&[window(0x30, "chat"), window(0x31, "mail")], 0x30),
                            ],
                            "monocle_container": null,
                            "maximized_window": null,
                            "floating_windows": [],
                        },
                        {
                            "name": "2",
                            "layout": "Columns",
                            "focused_container": 0,
                            "containers": [],
                            "monocle_container": null,
                            "maximized_window": null,
                            "floating_windows": [window(0x40, "calculator")],
                        },
                    ],
                },
                {
                    "focused_workspace": 0,
                    "workspaces": [
                        {
                            "name": "3",
                            "layout": "Rows",
                            "focused_container": 0,
                            "containers": [],
                            "monocle_container": container(&[window(0x50, "player")], 0x50),
                            "maximized_window": null,
                            "floating_windows": [],
                        },
                    ],
                },
            ],
        })
    }

    fn checker() -> Arc<Checker> {
        Arc::new(|path: Option<PathBuf>| match path {
            Some(p) if p.to_string_lossy().contains("missing") => {
                Err(format!("{} does not exist", p.display()))
            }
            p => Ok(json!({
                "version": "0.1.0",
                "path": p.map_or_else(
                    || "C:\\Users\\you\\mochi.json".to_owned(),
                    |p| p.display().to_string(),
                ),
                "source": "the default path",
                "kind": "config",
                "usable": true,
                "headline": null,
                "errors": [],
                "warnings": [],
                "notes": [],
            })),
        })
    }

    fn next() -> Command {
        Command::CycleFocus {
            direction: CycleDirection::Next,
        }
    }

    // The tools, one at a time
    // ------------------------

    #[test]
    fn a_window_is_found_wherever_the_state_keeps_it() {
        let state = desktop(0, 0, 0, 0x10);
        let at = |hwnd| locate(&state, hwnd).map(|(place, _)| place);
        assert_eq!(
            at(0x31),
            Some(Place {
                monitor: 0,
                workspace: 0,
                spot: Spot::Tiled {
                    container: 2,
                    position: 1,
                    stacked: true,
                },
            })
        );
        assert_eq!(at(0x40).unwrap().spot, Spot::Floating);
        assert_eq!(
            at(0x50),
            Some(Place {
                monitor: 1,
                workspace: 0,
                spot: Spot::Monocle,
            })
        );
        assert_eq!(at(0x77), None);
    }

    #[test]
    fn focusing_a_window_in_view_steps_along_the_ring() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10), desktop(0, 0, 1, 0x20)]);
        let answer = focus_window(&fake, &FocusParams { window: 0x20 }).unwrap();
        assert_eq!(fake.sent(), [Command::State, next(), Command::State]);
        assert_eq!(
            answer,
            json!({
                "ok": true,
                "window": 0x20,
                "monitor": 0,
                "workspace": 0,
                "sent": ["cycle-focus"],
            })
        );
    }

    #[test]
    fn focusing_a_window_in_a_stack_picks_its_place_in_the_stack() {
        let fake = Fake::with_states([desktop(0, 0, 1, 0x20), desktop(0, 0, 2, 0x31)]);
        focus_window(&fake, &FocusParams { window: 0x31 }).unwrap();
        assert_eq!(
            fake.sent(),
            [
                Command::State,
                next(),
                Command::FocusStackWindow { index: 1 },
                Command::State,
            ]
        );
    }

    #[test]
    fn focusing_a_window_elsewhere_switches_monitor_and_workspace_first() {
        // The focus is on monitor 1, and monitor 0 shows workspace 1. 0x10 is
        // on monitor 0, workspace 0, which remembers its third container as
        // focused, so the ring wraps once.
        let fake = Fake::with_states([
            desktop(1, 1, 2, 0x50),
            desktop(0, 0, 2, 0x30),
            desktop(0, 0, 0, 0x10),
        ]);
        focus_window(&fake, &FocusParams { window: 0x10 }).unwrap();
        assert_eq!(
            fake.sent(),
            [
                Command::State,
                Command::FocusMonitor { index: 0 },
                Command::FocusWorkspace { index: 0 },
                Command::State,
                next(),
                Command::State,
            ]
        );
    }

    #[test]
    fn a_monitor_that_already_shows_the_workspace_keeps_it() {
        let fake = Fake::with_states([desktop(1, 0, 0, 0x50), desktop(0, 0, 0, 0x10)]);
        focus_window(&fake, &FocusParams { window: 0x10 }).unwrap();
        assert_eq!(
            fake.sent(),
            [
                Command::State,
                Command::FocusMonitor { index: 0 },
                Command::State,
                Command::State,
            ]
        );
    }

    #[test]
    fn the_ring_is_walked_the_short_way_round() {
        let fake = Fake::with_states([desktop(0, 0, 2, 0x30), desktop(0, 0, 1, 0x20)]);
        focus_window(&fake, &FocusParams { window: 0x20 }).unwrap();
        assert_eq!(
            fake.sent(),
            [
                Command::State,
                Command::CycleFocus {
                    direction: CycleDirection::Previous,
                },
                Command::State,
            ]
        );
    }

    #[test]
    fn the_focused_window_needs_no_command() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10)]);
        let answer = focus_window(&fake, &FocusParams { window: 0x10 }).unwrap();
        assert_eq!(fake.sent(), [Command::State]);
        assert_eq!(answer["sent"], json!([]));
    }

    #[test]
    fn a_floating_window_is_shown_and_the_limit_is_said() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10), desktop(0, 1, 0, 0x10)]);
        let message = focus_window(&fake, &FocusParams { window: 0x40 }).unwrap_err();
        assert_eq!(
            fake.sent(),
            [
                Command::State,
                Command::FocusWorkspace { index: 1 },
                Command::State,
                Command::State,
            ]
        );
        assert!(message.contains("no command"), "{message}");
        assert!(
            message.contains("already sent: focus-workspace"),
            "{message}"
        );
    }

    #[test]
    fn an_unknown_window_is_named_in_the_error() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10)]);
        let message = focus_window(&fake, &FocusParams { window: 0x77 }).unwrap_err();
        assert!(message.contains("119"), "{message}");
        assert_eq!(fake.sent(), [Command::State]);
    }

    #[test]
    fn moving_sends_or_moves_by_index_or_by_name() {
        let cases = [
            (Some(3), None, false, Command::SendToWorkspace { index: 3 }),
            (Some(3), None, true, Command::MoveToWorkspace { index: 3 }),
            (
                None,
                Some("web"),
                false,
                Command::SendToNamedWorkspace { name: "web".into() },
            ),
            (
                None,
                Some("web"),
                true,
                Command::MoveToNamedWorkspace { name: "web".into() },
            ),
        ];
        for (workspace, name, follow, command) in cases {
            let fake = Fake::with_states([desktop(0, 0, 0, 0x10)]);
            let p = MoveParams {
                window: None,
                workspace,
                workspace_name: name.map(str::to_owned),
                follow,
            };
            let answer = move_window_to_workspace(&fake, &p).unwrap();
            assert_eq!(fake.sent(), std::slice::from_ref(&command));
            assert_eq!(answer["followed"], follow);
            assert_eq!(answer["sent"], json!([command.name()]));
        }
    }

    #[test]
    fn moving_a_given_window_focuses_it_first() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10), desktop(0, 0, 1, 0x20)]);
        let p = MoveParams {
            window: Some(0x20),
            workspace: Some(1),
            workspace_name: None,
            follow: false,
        };
        let answer = move_window_to_workspace(&fake, &p).unwrap();
        assert_eq!(
            fake.sent().last(),
            Some(&Command::SendToWorkspace { index: 1 })
        );
        assert_eq!(answer["sent"], json!(["cycle-focus", "send-to-workspace"]));
        assert_eq!(answer["window"], 0x20);
        assert_eq!(answer["workspace"], 1);
    }

    #[test]
    fn moving_needs_exactly_one_workspace() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10)]);
        for (workspace, name) in [(None, None), (Some(1), Some("web".to_owned()))] {
            let p = MoveParams {
                window: None,
                workspace,
                workspace_name: name,
                follow: false,
            };
            assert!(move_window_to_workspace(&fake, &p).is_err());
        }
        assert!(fake.sent().is_empty());
    }

    #[test]
    fn switching_maps_to_the_workspace_commands() {
        let fake = Fake::default();
        let p = SwitchParams {
            workspace: Some(2),
            monitor: Some(1),
            name: None,
        };
        switch_workspace(&fake, &p).unwrap();
        let p = SwitchParams {
            workspace: Some(4),
            monitor: None,
            name: None,
        };
        switch_workspace(&fake, &p).unwrap();
        let p = SwitchParams {
            workspace: None,
            monitor: None,
            name: Some("web".into()),
        };
        let answer = switch_workspace(&fake, &p).unwrap();
        assert_eq!(
            fake.sent(),
            [
                Command::FocusMonitor { index: 1 },
                Command::FocusWorkspace { index: 2 },
                Command::FocusWorkspace { index: 4 },
                Command::FocusNamedWorkspace { name: "web".into() },
            ]
        );
        assert_eq!(
            answer,
            json!({ "ok": true, "sent": ["focus-named-workspace"] })
        );
    }

    #[test]
    fn every_layout_name_is_one_the_daemon_takes() {
        for name in LayoutName::ALL {
            let fake = Fake::default();
            let answer = set_layout(&fake, &LayoutParams { layout: name }).unwrap();
            let layout = name.wire().unwrap();
            assert_eq!(fake.sent(), [Command::ChangeLayout { layout }]);
            assert_eq!(answer["layout"], name.name());
            let parsed: LayoutName = serde_json::from_value(json!(name.name())).unwrap();
            assert_eq!(parsed, name);
        }
    }

    #[test]
    fn toggles_act_on_the_focused_window_or_on_the_one_given() {
        let fake = Fake::default();
        let answer = toggle(&fake, &WindowParams { window: None }, Command::ToggleFloat).unwrap();
        assert_eq!(fake.sent(), [Command::ToggleFloat]);
        assert_eq!(
            answer,
            json!({ "ok": true, "window": null, "sent": ["toggle-float"] })
        );

        let fake = Fake::with_states([desktop(0, 0, 0, 0x10), desktop(0, 0, 1, 0x20)]);
        toggle(
            &fake,
            &WindowParams { window: Some(0x20) },
            Command::ToggleMonocle,
        )
        .unwrap();
        assert_eq!(fake.sent().last(), Some(&Command::ToggleMonocle));
    }

    #[test]
    fn pausing_and_resuming_only_toggle_when_needed() {
        let paused_query = Command::Query {
            target: QueryTarget::Paused,
        };
        let running = Fake::default();
        let answer = set_paused(&running, true).unwrap();
        assert_eq!(running.sent(), [paused_query.clone(), Command::TogglePause]);
        assert_eq!(
            answer,
            json!({ "ok": true, "paused": true, "changed": true, "sent": ["toggle-pause"] })
        );

        let paused = Fake {
            paused: true,
            ..Fake::default()
        };
        let answer = set_paused(&paused, true).unwrap();
        assert_eq!(answer["changed"], false);
        assert_eq!(paused.sent(), [paused_query]);
        set_paused(&paused, false).unwrap();
        assert_eq!(paused.sent().last(), Some(&Command::TogglePause));
    }

    #[test]
    fn why_answers_for_a_managed_window_from_the_state() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10)]);
        let answer = why_not_tiled(&fake, WhyParams { window: Some(0x31) }).unwrap();
        assert_eq!(
            answer,
            json!({
                "hwnd": "0x31",
                "title": "mail",
                "exe": "mail.exe",
                "class": "C",
                "managed": true,
                "monitor": 0,
                "workspace": 0,
            })
        );
        assert_eq!(fake.sent(), [Command::State]);
    }

    #[test]
    fn why_asks_the_daemon_about_the_window_in_front() {
        let fake = Fake::with_states([desktop(0, 0, 0, 0x10)]);
        let front = why_not_tiled(&fake, WhyParams { window: None }).unwrap();
        assert_eq!(front["reason"], "tool window");
        assert_eq!(fake.sent(), [Command::Why]);

        let by_id = why_not_tiled(&fake, WhyParams { window: Some(0x99) }).unwrap();
        assert_eq!(by_id, front);

        let message = why_not_tiled(&fake, WhyParams { window: Some(0x77) }).unwrap_err();
        assert!(message.contains("in front"), "{message}");
    }

    #[test]
    fn a_refusal_comes_back_in_the_daemons_words() {
        let fake = Fake {
            refuse: Some(("reload-configuration", "mochi.json: expected `,` at line 4")),
            ..Fake::default()
        };
        let message = simple(&fake, Command::ReloadConfiguration).unwrap_err();
        assert_eq!(
            message,
            "Mochi refused `reload-configuration`: mochi.json: expected `,` at line 4"
        );
    }

    // Over the protocol
    // -----------------

    /// A client on the other end of an in-memory stdio pipe.
    struct Session {
        write: WriteHalf<DuplexStream>,
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        server: tokio::task::JoinHandle<()>,
        next_id: u64,
    }

    impl Session {
        async fn open(server: Server) -> (Session, Value) {
            let (ours, theirs) = tokio::io::duplex(1 << 20);
            let server = tokio::spawn(async move {
                if let Ok(running) = server.serve(theirs).await {
                    let _ = running.waiting().await;
                }
            });
            let (read, write) = tokio::io::split(ours);
            let mut session = Session {
                write,
                lines: BufReader::new(read).lines(),
                server,
                next_id: 0,
            };
            let init = session
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": { "name": "test", "version": "1" },
                    }),
                )
                .await;
            session
                .send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .await;
            (session, init)
        }

        async fn send(&mut self, message: Value) {
            let line = format!("{message}\n");
            self.write.write_all(line.as_bytes()).await.unwrap();
        }

        /// Sends a request and returns the whole answer to it.
        async fn request(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let id = self.next_id;
            self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
                .await;
            loop {
                let line = tokio::time::timeout(Duration::from_secs(20), self.lines.next_line())
                    .await
                    .expect("an answer in time")
                    .unwrap()
                    .expect("the server is still there");
                let answer: Value = serde_json::from_str(&line).expect("one JSON message a line");
                if answer["id"] == json!(id) {
                    return answer;
                }
            }
        }

        /// Calls a tool and returns its result.
        async fn call(&mut self, tool: &str, arguments: Value) -> Value {
            let answer = self
                .request(
                    "tools/call",
                    json!({ "name": tool, "arguments": arguments }),
                )
                .await;
            assert!(answer.get("error").is_none(), "{answer}");
            answer["result"].clone()
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    const TOOLS: [&str; 14] = [
        "check_config",
        "focus_window",
        "get_state",
        "list_hotkeys",
        "move_window_to_workspace",
        "pause_tiling",
        "reload_config",
        "resume_tiling",
        "retile",
        "set_layout",
        "switch_workspace",
        "toggle_float",
        "toggle_monocle",
        "why_not_tiled",
    ];

    const READ_ONLY: [&str; 4] = ["check_config", "get_state", "list_hotkeys", "why_not_tiled"];

    fn server(fake: Fake) -> (Server, Arc<Fake>) {
        let fake = Arc::new(fake);
        let daemon: Arc<dyn Daemon> = fake.clone();
        (Server::new(daemon, checker()), fake)
    }

    #[tokio::test]
    async fn the_handshake_names_the_server_and_offers_tools() {
        let (server, _) = server(Fake::default());
        let (_session, init) = Session::open(server).await;
        let result = &init["result"];
        assert_eq!(result["serverInfo"]["name"], "mochi", "{init}");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(result["capabilities"]["tools"].is_object(), "{init}");
        assert!(
            result["instructions"]
                .as_str()
                .unwrap()
                .contains("get_state")
        );
    }

    #[tokio::test]
    async fn tools_are_listed_with_their_arguments_and_hints() {
        let (server, _) = server(Fake::default());
        let (mut session, _) = Session::open(server).await;
        let answer = session.request("tools/list", json!({})).await;
        let tools = answer["result"]["tools"]
            .as_array()
            .expect("a list")
            .clone();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        assert_eq!(names, TOOLS);
        for tool in &tools {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(!tool["description"].as_str().unwrap().is_empty(), "{tool}");
            assert_eq!(
                tool["annotations"]["readOnlyHint"] == json!(true),
                READ_ONLY.contains(&name),
                "{tool}"
            );
            assert_ne!(
                tool["annotations"]["destructiveHint"],
                json!(true),
                "{tool}"
            );
        }
        let tool = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap().clone();
        assert_eq!(
            tool("focus_window")["inputSchema"]["required"],
            json!(["window"])
        );
        assert_eq!(
            tool("set_layout")["inputSchema"]["required"],
            json!(["layout"])
        );
        let layouts = tool("set_layout")["inputSchema"].to_string();
        for name in LayoutName::ALL {
            assert!(
                layouts.contains(&format!("\"{}\"", name.name())),
                "{layouts}"
            );
        }
    }

    #[tokio::test]
    async fn reading_tools_answer_with_the_command_lines_documents() {
        let state = desktop(0, 0, 0, 0x10);
        let (server, _) = server(Fake::with_states([state.clone()]));
        let (mut session, _) = Session::open(server).await;

        let answer = session.call("get_state", json!({})).await;
        assert_eq!(answer["structuredContent"], state);
        // The text is the same JSON, for clients that read only text.
        let text: Value =
            serde_json::from_str(answer["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, state);

        let hotkeys = session.call("list_hotkeys", json!({})).await;
        assert_eq!(
            hotkeys["structuredContent"]["bindings"][0]["command"],
            "focus left"
        );

        let check = session.call("check_config", json!({})).await;
        assert_eq!(check["structuredContent"]["usable"], true);
        let check = session
            .call("check_config", json!({ "path": "D:\\mochi.json" }))
            .await;
        assert_eq!(check["structuredContent"]["path"], "D:\\mochi.json");
        let missing = session
            .call("check_config", json!({ "path": "D:\\missing.json" }))
            .await;
        assert_eq!(missing["isError"], true, "{missing}");
    }

    #[tokio::test]
    async fn an_action_answers_with_what_it_sent() {
        let (server, fake) = server(Fake::default());
        let (mut session, _) = Session::open(server).await;
        let answer = session
            .call("set_layout", json!({ "layout": "vertical-stack" }))
            .await;
        assert_eq!(
            answer["structuredContent"],
            json!({ "ok": true, "layout": "vertical-stack", "sent": ["change-layout"] })
        );
        assert_eq!(
            fake.sent(),
            [Command::ChangeLayout {
                layout: Layout::VerticalStack,
            }]
        );
    }

    #[tokio::test]
    async fn no_tool_stops_mochi_or_writes_a_file() {
        let (server, fake) = server(Fake::with_states([
            desktop(1, 0, 2, 0x50),
            desktop(0, 0, 0, 0x30),
        ]));
        let (mut session, _) = Session::open(server).await;
        for tool in TOOLS {
            session.call(tool, arguments(tool)).await;
        }
        for command in fake.sent() {
            assert!(
                !matches!(
                    command,
                    Command::Stop | Command::Start | Command::Quickstart | Command::Close
                ),
                "{command:?}"
            );
        }
    }

    /// Arguments each tool accepts.
    fn arguments(tool: &str) -> Value {
        match tool {
            "focus_window" => json!({ "window": 0x31 }),
            "move_window_to_workspace" | "switch_workspace" => json!({ "workspace": 1 }),
            "set_layout" => json!({ "layout": "bsp" }),
            _ => json!({}),
        }
    }

    #[tokio::test]
    async fn every_tool_says_how_to_start_mochi_when_it_is_not_running() {
        let (server, _) = server(Fake {
            stopped: true,
            ..Fake::default()
        });
        let (mut session, _) = Session::open(server).await;
        for tool in TOOLS {
            let answer = session.call(tool, arguments(tool)).await;
            if tool == "check_config" {
                // Checking a file needs no daemon: it is the tool for when
                // Mochi will not start.
                assert_ne!(answer["isError"], true, "{answer}");
                continue;
            }
            assert_eq!(answer["isError"], true, "{tool}: {answer}");
            assert_eq!(answer["content"][0]["text"], NOT_RUNNING, "{tool}");
        }
    }
}
