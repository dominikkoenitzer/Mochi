//! A monitor: its geometry and the workspaces that live on it.

use serde::{Deserialize, Serialize};

use crate::geometry::{Offset, Rect};

use super::ring::Ring;
use super::window::WindowId;
use super::workspace::Workspace;

/// The DPI of a display that has not been probed yet.
pub const DEFAULT_DPI: u32 = 96;

/// One display, with its own ring of workspaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Monitor {
    /// The platform handle, an `HMONITOR` on Windows.
    pub id: isize,
    /// The friendly name, for example `DISPLAY1`.
    pub name: String,
    /// The GDI device name, for example `\\.\DISPLAY1`.
    pub device: String,
    /// The stable device id used by `display_index_preferences`.
    pub device_id: String,
    /// The full display rectangle in virtual desktop coordinates.
    pub size: Rect,
    /// The part of `size` that is not covered by the taskbar.
    pub work_area: Rect,
    /// The display's DPI. 96 is 100 percent scaling.
    pub dpi: u32,
    /// The workspaces on this monitor.
    workspaces: Ring<Workspace>,
    /// The workspace that was focused before the current one, for
    /// `focus-last-workspace`.
    pub last_focused_workspace: Option<usize>,
    /// An extra offset applied to the work area of every workspace here.
    pub work_area_offset: Option<Offset>,
    /// An offset applied only to workspaces that opt in with
    /// [`Workspace::apply_window_based_work_area_offset`].
    pub window_based_work_area_offset: Option<Offset>,
    /// The offset above only applies while the workspace holds at most this
    /// many containers.
    pub window_based_work_area_offset_limit: usize,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            device: String::new(),
            device_id: String::new(),
            size: Rect::default(),
            work_area: Rect::default(),
            dpi: DEFAULT_DPI,
            workspaces: Ring::new(),
            last_focused_workspace: None,
            work_area_offset: None,
            window_based_work_area_offset: None,
            window_based_work_area_offset_limit: 1,
        }
    }
}

impl Monitor {
    /// A monitor with one empty workspace.
    #[must_use]
    pub fn new(id: isize, size: Rect, work_area: Rect) -> Self {
        Self {
            id,
            size,
            work_area,
            workspaces: Ring::from_vec(vec![Workspace::new()]),
            ..Self::default()
        }
    }

    /// Sets the friendly name, builder style.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets the device and device id, builder style.
    #[must_use]
    pub fn with_device(mut self, device: impl Into<String>, device_id: impl Into<String>) -> Self {
        self.device = device.into();
        self.device_id = device_id.into();
        self
    }

    /// Sets the DPI, builder style.
    #[must_use]
    pub fn with_dpi(mut self, dpi: u32) -> Self {
        self.dpi = dpi;
        self
    }

    /// The scaling factor, where 1.0 means 96 DPI.
    #[must_use]
    pub fn scale_factor(&self) -> f32 {
        if self.dpi == 0 {
            return 1.0;
        }
        self.dpi as f32 / DEFAULT_DPI as f32
    }

    /// `true` when the display is taller than it is wide.
    #[must_use]
    pub fn is_portrait(&self) -> bool {
        self.size.height() > self.size.width()
    }

    /// The workspaces on this monitor.
    #[must_use]
    pub fn workspaces(&self) -> &Ring<Workspace> {
        &self.workspaces
    }

    /// The workspaces, mutably.
    pub fn workspaces_mut(&mut self) -> &mut Ring<Workspace> {
        &mut self.workspaces
    }

    /// The index of the focused workspace.
    #[must_use]
    pub fn focused_workspace_idx(&self) -> usize {
        self.workspaces.focused_idx()
    }

    /// The focused workspace.
    #[must_use]
    pub fn focused_workspace(&self) -> Option<&Workspace> {
        self.workspaces.focused()
    }

    /// The focused workspace, mutably.
    pub fn focused_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.focused_mut()
    }

    /// Focuses a workspace by index, remembering the one we came from.
    ///
    /// Returns `false` when the index is out of range.
    pub fn focus_workspace(&mut self, idx: usize) -> bool {
        let previous = self.workspaces.focused_idx();
        if !self.workspaces.focus(idx) {
            return false;
        }
        if previous != idx {
            self.last_focused_workspace = Some(previous);
        }
        true
    }

    /// Makes sure there are at least `count` workspaces, creating empty ones
    /// with numeric names as needed.
    ///
    /// Returns the number that were created.
    pub fn ensure_workspaces(&mut self, count: usize) -> usize {
        let mut created = 0;
        while self.workspaces.len() < count {
            self.workspaces.push(Workspace::new());
            created += 1;
        }
        created
    }

    /// The name of a workspace, falling back to its one-based index.
    #[must_use]
    pub fn workspace_name(&self, idx: usize) -> String {
        self.workspaces
            .get(idx)
            .and_then(|w| w.name.clone())
            .unwrap_or_else(|| (idx + 1).to_string())
    }

    /// The area a workspace tiles into, with every configured offset applied.
    #[must_use]
    pub fn work_area_for(&self, idx: usize, global_offset: Option<Offset>) -> Rect {
        let mut area = self.work_area;
        if let Some(offset) = self.work_area_offset.or(global_offset) {
            area = offset.apply(area);
        }
        if let Some(workspace) = self.workspaces.get(idx)
            && workspace.apply_window_based_work_area_offset
            && workspace.containers().len() <= self.window_based_work_area_offset_limit
            && let Some(offset) = self.window_based_work_area_offset
        {
            area = offset.apply(area);
        }
        area
    }

    /// The index of the workspace holding a window.
    #[must_use]
    pub fn workspace_idx_for_window(&self, id: WindowId) -> Option<usize> {
        self.workspaces.position(|w| w.contains_window(id))
    }

    /// `true` when any workspace on this monitor holds the window.
    #[must_use]
    pub fn contains_window(&self, id: WindowId) -> bool {
        self.workspace_idx_for_window(id).is_some()
    }

    /// Every window handle on this monitor.
    pub fn all_window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.workspaces.iter().flat_map(Workspace::all_window_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Window;

    const SIZE: Rect = Rect::new(0, 0, 3840, 2160);
    const WORK_AREA: Rect = Rect::new(0, 0, 3840, 2120);

    fn monitor() -> Monitor {
        let mut m = Monitor::new(1, SIZE, WORK_AREA)
            .with_name("DISPLAY1")
            .with_device(r"\\.\DISPLAY1", "G8-12345")
            .with_dpi(144);
        m.ensure_workspaces(9);
        m
    }

    #[test]
    fn a_new_monitor_has_one_workspace() {
        let m = Monitor::new(1, SIZE, WORK_AREA);
        assert_eq!(m.workspaces().len(), 1);
        assert_eq!(m.focused_workspace_idx(), 0);
        assert!(m.focused_workspace().is_some());
        assert_eq!(m.dpi, DEFAULT_DPI);
        assert_eq!(m.last_focused_workspace, None);
    }

    #[test]
    fn builders_fill_the_identity() {
        let m = monitor();
        assert_eq!(m.name, "DISPLAY1");
        assert_eq!(m.device, r"\\.\DISPLAY1");
        assert_eq!(m.device_id, "G8-12345");
        assert_eq!(m.dpi, 144);
    }

    #[test]
    fn scale_factor_follows_the_dpi() {
        assert!((Monitor::default().scale_factor() - 1.0).abs() < f32::EPSILON);
        assert!((monitor().scale_factor() - 1.5).abs() < f32::EPSILON);
        let broken = Monitor {
            dpi: 0,
            ..Monitor::default()
        };
        assert!((broken.scale_factor() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn portrait_displays_are_recognised() {
        assert!(!monitor().is_portrait());
        let portrait = Monitor::new(2, Rect::new(0, 0, 1080, 1920), Rect::new(0, 0, 1080, 1920));
        assert!(portrait.is_portrait());
    }

    #[test]
    fn ensure_workspaces_only_ever_adds() {
        let mut m = Monitor::new(1, SIZE, WORK_AREA);
        assert_eq!(m.ensure_workspaces(9), 8);
        assert_eq!(m.workspaces().len(), 9);
        assert_eq!(m.ensure_workspaces(4), 0, "never removes");
        assert_eq!(m.workspaces().len(), 9);
    }

    #[test]
    fn focusing_a_workspace_remembers_the_previous_one() {
        let mut m = monitor();
        assert!(m.focus_workspace(3));
        assert_eq!(m.focused_workspace_idx(), 3);
        assert_eq!(m.last_focused_workspace, Some(0));

        assert!(m.focus_workspace(5));
        assert_eq!(m.last_focused_workspace, Some(3));

        assert!(m.focus_workspace(5));
        assert_eq!(m.last_focused_workspace, Some(3), "no self reference");

        assert!(!m.focus_workspace(99));
        assert_eq!(m.focused_workspace_idx(), 5);
    }

    #[test]
    fn workspace_names_fall_back_to_the_index() {
        let mut m = monitor();
        m.workspaces_mut().get_mut(0).unwrap().name = Some("code".into());
        assert_eq!(m.workspace_name(0), "code");
        assert_eq!(m.workspace_name(4), "5");
        assert_eq!(m.workspace_name(99), "100");
    }

    #[test]
    fn work_area_offsets_stack_up() {
        let mut m = monitor();
        assert_eq!(m.work_area_for(0, None), WORK_AREA);

        assert_eq!(
            m.work_area_for(0, Some(Offset::new(0, 40, 0, 0))),
            Rect::new(0, 40, 3840, 2120),
            "the global offset applies when the monitor has none"
        );

        m.work_area_offset = Some(Offset::new(10, 10, 10, 10));
        assert_eq!(
            m.work_area_for(0, Some(Offset::new(0, 400, 0, 0))),
            Rect::new(10, 10, 3830, 2110),
            "the monitor offset overrides the global one"
        );
    }

    #[test]
    fn the_window_based_offset_needs_the_opt_in_and_the_limit() {
        let mut m = monitor();
        m.window_based_work_area_offset = Some(Offset::new(100, 0, 100, 0));
        assert_eq!(m.work_area_for(0, None), WORK_AREA, "not opted in");

        m.workspaces_mut()
            .get_mut(0)
            .unwrap()
            .apply_window_based_work_area_offset = true;
        assert_eq!(m.work_area_for(0, None), Rect::new(100, 0, 3740, 2120));

        let ws = m.workspaces_mut().get_mut(0).unwrap();
        ws.add_window(Window::new(1));
        ws.add_window(Window::new(2));
        assert_eq!(
            m.work_area_for(0, None),
            WORK_AREA,
            "over the container limit"
        );
    }

    #[test]
    fn windows_are_found_by_workspace() {
        let mut m = monitor();
        m.workspaces_mut()
            .get_mut(2)
            .unwrap()
            .add_window(Window::new(7));
        assert_eq!(m.workspace_idx_for_window(WindowId(7)), Some(2));
        assert!(m.contains_window(WindowId(7)));
        assert!(!m.contains_window(WindowId(8)));
        assert_eq!(m.all_window_ids().collect::<Vec<_>>(), vec![WindowId(7)]);
    }

    #[test]
    fn round_trips_through_json() {
        let m = monitor();
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Monitor>(&json).unwrap(), m);
    }
}
