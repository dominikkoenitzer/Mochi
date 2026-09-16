//! The window manager tree: monitor, workspace, container, window.
//!
//! Every level is a [`Ring`]: an ordered list that remembers which element is
//! focused. A [`State`] holds a ring of [`Monitor`]s, each monitor a ring of
//! [`Workspace`]s, each workspace a ring of [`Container`]s, and each container
//! a ring of [`Window`]s of which only the focused one is visible.
//!
//! Nothing here touches Win32. A [`Window`] is an opaque [`WindowId`] plus the
//! metadata the daemon caches for it, and every mutating operation reports what
//! it changed through [`Changes`].

mod changes;
mod container;
mod monitor;
mod ring;
mod state;
mod window;
mod workspace;

pub use changes::{Changes, WorkspaceRef};
pub use container::Container;
pub use monitor::{DEFAULT_DPI, Monitor, scale_padding};
pub use ring::{CycleDirection, Ring};
pub use state::{
    FocusFollowsMouseImplementation, HidingBehaviour, MoveBehaviour, OperationBehaviour, State,
    WindowContainerBehaviour, WindowManager,
};
pub use window::{Window, WindowId};
pub use workspace::Workspace;
