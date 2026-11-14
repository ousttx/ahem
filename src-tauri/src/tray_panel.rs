//! The quick panel: a small window next to the tray icon with the state, "Recognize again",
//! recording a meeting, the mode, the microphone, the model, the last text, the recent
//! dictations and the day's statistics. A left click on the icon toggles it, the tray menu
//! opens it too (some Linux desktops send no clicks), and it hides when it loses focus. The
//! full window stays for the setup, models, modes and the rest.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, LogicalSize, Manager, PhysicalPosition, Position, Rect, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::platform;

pub const LABEL: &str = "tray";
/// The panel's width in CSS pixels (`tray.css`).
const PANEL_WIDTH: f64 = 360.0;
/// The page's width in CSS pixels, its transparent margin around the panel included.
const WIDTH: f64 = PANEL_WIDTH + 2.0 * platform::TRAY_PANEL_MARGIN;
/// Until the page reports its height.
const INITIAL_HEIGHT: f64 = 640.0;
/// A click on the icon that hid the panel by taking the focus must not show it again.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Top-left corner of the panel: centered on the icon, or with its left edge `left_of_icon`
/// physical pixels left of the icon's (as menus hang from the macOS menu bar); below an icon in
/// the upper half of the screen (a menu bar, a top panel) and above it otherwise (a taskbar),
/// inside the work area.
pub fn panel_position(
    icon: Area,
    panel: (i32, i32),
    work: Area,
    left_of_icon: Option<i32>,
) -> (i32, i32) {
    let (width, height) = panel;
    let max_x = (work.x + work.width - width).max(work.x);
    let x = match left_of_icon {
        Some(offset) => icon.x - offset,
        None => icon.x + icon.width / 2 - width / 2,
    }
    .clamp(work.x, max_x);
    let below = icon.y + icon.height / 2 < work.y + work.height / 2;
    let y = if below { icon.y + icon.height } else { icon.y - height };
    let max_y = (work.y + work.height - height).max(work.y);
    (x, y.clamp(work.y, max_y))
}

/// The window's height in logical pixels: the page's, at most the work area's (physical pixels
/// at the monitor's `scale`); a taller page scrolls (`tray.css`).
pub fn fitted_height(page: f64, work: Area, scale: f64) -> f64 {
    page.min(f64::from(work.height) / scale)
}

struct Anchor {
    icon: Area,
    work: Area,
    /// The scale of the monitor of the work area.
    scale: f64,
}

pub struct TrayPanel {
    window: WebviewWindow,
    anchor: Mutex<Option<Anchor>>,
    hidden_at: Mutex<Option<Instant>>,
    /// The page's height in CSS pixels, as it last reported it.
    height: Mutex<f64>,
    /// The program in front before the panel took the focus, to get it back when the panel is
    /// put away (`dismiss`).
    previous: Mutex<FocusReturn>,
    /// Shown, as the panel itself knows: a hotkey's handler must not ask the main thread (on
    /// Linux it runs on the thread the main thread waits for while hotkeys are registered).
    shown: AtomicBool,
}

/// Which program gets the focus back when the panel is put away: the one in front when the
/// panel opened, not ahem itself once the panel is shown, and only once.
#[derive(Debug, Default)]
struct FocusReturn(Option<u32>);

impl FocusReturn {
    /// The panel is shown; `front` is the program in front now. Shown again while open (from the
    /// tray menu), the program in front may be another by then: the program from before stays.
    fn shown(&mut self, front: Option<u32>, was_visible: bool) {
        if !was_visible {
            self.0 = front;
        }
    }

    /// Put away by the user: the program to bring forward, once.
    fn dismissed(&mut self) -> Option<u32> {
        self.0.take()
    }
}

impl TrayPanel {
    pub fn create(app: &AppHandle) -> tauri::Result<Self> {
        let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("tray.html".into()))
            .title("ahem")
            .inner_size(WIDTH * platform::UI_ZOOM, INITIAL_HEIGHT * platform::UI_ZOOM)
            .decorations(false)
            .shadow(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            // On macOS this needs `macOSPrivateApi` (tauri.conf.json).
            .transparent(true)
            .additional_browser_args(platform::BROWSER_ARGS);
        let window = platform::build_floating(|| builder.build())?;
        if let Err(error) = window.set_zoom(platform::UI_ZOOM) {
            log_line!("cannot zoom the tray panel: {error}");
        }
        Ok(Self {
            window,
            anchor: Mutex::new(None),
            hidden_at: Mutex::new(None),
            height: Mutex::new(INITIAL_HEIGHT),
            previous: Mutex::new(FocusReturn::default()),
            shown: AtomicBool::new(false),
        })
    }

    /// A left click on the icon: show the panel under or over it, or hide it.
    pub fn toggle(&self, icon: Rect) {
        if self.window.is_visible().unwrap_or(false) {
            self.dismiss();
            return;
        }
        let just_hidden = self
            .hidden_at
            .lock()
            .expect("tray panel lock")
            .is_some_and(|at| at.elapsed() < REOPEN_GUARD);
        if !just_hidden {
            self.show_at(icon_area(&self.window, icon));
        }
    }

    /// From the tray menu: next to the mouse, where the menu was.
    pub fn show_near_cursor(&self) {
        let Ok(cursor) = self.window.cursor_position() else { return };
        let point = Area { x: cursor.x as i32, y: cursor.y as i32, width: 0, height: 0 };
        self.show_at(Some(point));
    }

    /// Put away by a dictation hotkey, if it is shown, as by Escape: the program from before
    /// gets the focus back, if the panel took it.
    pub fn dismiss_if_shown(&self) {
        if self.shown.load(Ordering::SeqCst) {
            self.dismiss();
        }
    }

    pub fn hide(&self) {
        *self.hidden_at.lock().expect("tray panel lock") = Some(Instant::now());
        self.shown.store(false, Ordering::SeqCst);
        let _ = self.window.hide();
        platform::set_panel_keys(false);
    }

    /// Put away by the user (Escape, a second click on the icon), not by another window taking
    /// the focus: the program in front before gets the focus back, so a dictation right after
    /// goes there and not into ahem.
    pub fn dismiss(&self) {
        self.hide();
        if let Some(pid) = self.previous.lock().expect("tray panel lock").dismissed() {
            platform::give_focus_back(pid);
        }
    }

    /// The page reports its height in CSS pixels; the panel keeps its place at the icon.
    pub fn fit(&self, height: f64) {
        *self.height.lock().expect("tray panel lock") = height;
        self.refit();
    }

    /// The size and the place again, for the page's last height: after it changed, or after the
    /// panel moved to a screen of another scale.
    pub fn refit(&self) {
        let anchor = self.anchor.lock().expect("tray panel lock");
        self.resize(anchor.as_ref());
        if let Some(anchor) = &*anchor {
            self.place(anchor);
        }
    }

    /// The page's height, at most the work area's once the panel is shown at the icon.
    fn resize(&self, anchor: Option<&Anchor>) {
        let page = *self.height.lock().expect("tray panel lock") * platform::UI_ZOOM;
        let height = anchor.map_or(page, |anchor| fitted_height(page, anchor.work, anchor.scale));
        let _ = self.window.set_size(LogicalSize::new(WIDTH * platform::UI_ZOOM, height));
    }

    fn show_at(&self, icon: Option<Area>) {
        let Some(icon) = icon else { return };
        let monitor = self
            .window
            .monitor_from_point(f64::from(icon.x), f64::from(icon.y))
            .ok()
            .flatten()
            .or_else(|| self.window.primary_monitor().ok().flatten());
        let Some(monitor) = monitor else { return };
        let area = monitor.work_area();
        let work = Area {
            x: area.position.x,
            y: area.position.y,
            width: i32::try_from(area.size.width).unwrap_or(i32::MAX),
            height: i32::try_from(area.size.height).unwrap_or(i32::MAX),
        };
        let anchor = Anchor { icon, work, scale: monitor.scale_factor() };
        self.resize(Some(&anchor));
        self.place(&anchor);
        *self.anchor.lock().expect("tray panel lock") = Some(anchor);
        let was_visible = self.window.is_visible().unwrap_or(false);
        let front = platform::frontmost_program();
        self.previous.lock().expect("tray panel lock").shown(front, was_visible);
        let _ = self.window.show();
        self.shown.store(true, Ordering::SeqCst);
        platform::focus_panel(&self.window);
    }

    fn place(&self, anchor: &Anchor) {
        let Ok(size) = self.window.outer_size() else { return };
        let panel = (
            i32::try_from(size.width).unwrap_or(i32::MAX),
            i32::try_from(size.height).unwrap_or(i32::MAX),
        );
        // The window's left edge: the panel's, less its margin (CSS pixels, zoomed to points).
        let margin = platform::TRAY_PANEL_MARGIN * platform::UI_ZOOM;
        let left_of_icon = platform::TRAY_PANEL_LEFT_OF_ICON
            .map(|points| ((points + margin) * anchor.scale).round() as i32);
        let (x, y) = panel_position(anchor.icon, panel, anchor.work, left_of_icon);
        let _ = self.window.set_position(PhysicalPosition { x, y });
    }
}

/// The icon's rectangle in physical pixels. The tray gives it so on every system; a logical one
/// is converted at the panel's scale.
fn icon_area(window: &WebviewWindow, icon: Rect) -> Option<Area> {
    let scale = match icon.position {
        Position::Physical(_) => 1.0,
        Position::Logical(_) => window.scale_factor().unwrap_or(1.0),
    };
    let position = icon.position.to_physical::<i32>(scale);
    let size = icon.size.to_physical::<u32>(scale);
    Some(Area {
        x: position.x,
        y: position.y,
        width: i32::try_from(size.width).ok()?,
        height: i32::try_from(size.height).ok()?,
    })
}

pub fn get(app: &AppHandle) -> Option<tauri::State<'_, TrayPanel>> {
    app.try_state::<TrayPanel>()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = Area { x: 0, y: 0, width: 2880, height: 1800 };

    /// The program in front when the panel opened gets the focus back once; showing the open
    /// panel again (from the tray menu, maybe with ahem in front by then) does not replace it.
    #[test]
    fn the_program_from_before_gets_the_focus_back_once() {
        let mut focus = FocusReturn::default();
        focus.shown(Some(42), false);
        focus.shown(Some(7), true);
        assert_eq!(focus.dismissed(), Some(42));
        assert_eq!(focus.dismissed(), None, "once");
        focus.shown(None, false);
        assert_eq!(focus.dismissed(), None);
    }

    #[test]
    fn hangs_under_a_menu_bar_icon() {
        // macOS: the menu bar is above the work area.
        let work = Area { x: 0, y: 74, width: 2880, height: 1726 };
        let icon = Area { x: 2400, y: 0, width: 44, height: 74 };
        assert_eq!(panel_position(icon, (784, 1200), work, None), (2030, 74));
        // As a menu of the menu bar: its left edge just left of the icon's, right under the bar.
        let middle = Area { x: 1000, ..icon };
        assert_eq!(panel_position(middle, (784, 1200), work, Some(12)), (988, 74));
        // Near the right edge it stays on the screen.
        assert_eq!(panel_position(icon, (784, 1200), work, Some(12)), (2096, 74));
    }

    #[test]
    fn stands_over_a_taskbar_icon() {
        // Windows: the taskbar is below the work area; the icon is near the right edge.
        let work = Area { x: 0, y: 0, width: 1920, height: 1032 };
        let icon = Area { x: 1800, y: 1040, width: 24, height: 40 };
        assert_eq!(panel_position(icon, (470, 760), work, None), (1450, 272));
    }

    #[test]
    fn stays_inside_the_work_area() {
        let icon = Area { x: 0, y: 0, width: 10, height: 10 };
        assert_eq!(panel_position(icon, (400, 600), SCREEN, None), (0, 10));
        let too_big = panel_position(icon, (4000, 4000), SCREEN, None);
        assert_eq!(too_big, (0, 0));
    }

    #[test]
    fn is_never_taller_than_the_work_area() {
        // 1920×1080 at 150 %: a work area of 1032 physical pixels is 688 logical ones.
        let work = Area { x: 0, y: 0, width: 1920, height: 1032 };
        assert_eq!(fitted_height(600.0, work, 1.5), 600.0);
        assert_eq!(fitted_height(800.0, work, 1.5), 688.0);
    }

    #[test]
    fn follows_the_mouse_from_the_menu() {
        // A zero-size point stands for the mouse where the tray menu was.
        let point = Area { x: 1500, y: 1700, width: 0, height: 0 };
        assert_eq!(panel_position(point, (400, 600), SCREEN, None), (1300, 1100));
    }
}
