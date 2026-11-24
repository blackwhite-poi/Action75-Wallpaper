use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, ffi::c_void, rc::Rc};
use windows::{
    Win32::{
        Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{
                DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
                DWMWA_SYSTEMBACKDROP_TYPE,
                DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
                DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
            },
            Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
                MonitorFromRect, ScreenToClient,
            },
        },
        System::{
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        },
        UI::{
            Controls::MARGINS,
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                DefWindowProcW, GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, GetWindowRect, HTBOTTOM,
                HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
                HTTOPRIGHT, IsZoomed, SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SM_CYSIZEFRAME,
                STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
                SetWindowLongW, SetWindowPos, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCDESTROY,
                WM_NCHITTEST, WM_NCPAINT, WM_STYLECHANGING, WS_CAPTION, WS_EX_CLIENTEDGE,
                WS_EX_DLGMODALFRAME, WS_EX_STATICEDGE, WS_EX_WINDOWEDGE, WS_POPUP, WS_SYSMENU,
            },
        },
    },
    core::{s, w},
};

fn enable_acrylic(hwnd: HWND) -> Result<&'static str, String> {
    let mut transparency = 1u32;
    let mut size = std::mem::size_of_val(&transparency) as u32;
    let _ = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("EnableTransparency"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut transparency).cast()),
            Some(&mut size),
        )
    };
    if transparency == 0 {
        return Err("Windows 已关闭透明效果".into());
    }
    let _ = set_dwm_value(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(true));
    // The caption styles have already been cleared. Now the full-client
    // material cannot reveal a second set of system buttons.
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) }
        .map_err(|error| format!("无法扩展 Acrylic 材质：{error}"))?;
    let _ = set_dwm_value(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
    if set_accent(hwnd, true) {
        return Ok("AccentPolicy");
    }
    set_dwm_value(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW)
        .map(|()| "DWM Desktop Acrylic")
        .map_err(|error| format!("无法启用 Desktop Acrylic：{error}"))
}

fn set_accent(hwnd: HWND, enabled: bool) -> bool {
    let apply_accent = unsafe { GetModuleHandleW(w!("user32.dll")) }.ok().and_then(|module| unsafe {
        GetProcAddress(module, s!("SetWindowCompositionAttribute"))
    });
    if let Some(set) = apply_accent {
        #[repr(C)]
        struct AccentPolicy {
            state: i32,
            flags: u32,
            color: u32,
            animation: u32,
        }
        #[repr(C)]
        struct CompositionAttribute {
            attribute: i32,
            data: *mut c_void,
            size: usize,
        }
        type SetComposition = unsafe extern "system" fn(HWND, *const CompositionAttribute) -> BOOL;
        // Final Spatial Audio uses this optional accent API to keep the material
        // visible when the window loses focus. Only one provider paints the backdrop.
        let set: SetComposition = unsafe { std::mem::transmute(set) };
        let mut accent = AccentPolicy {
            state: if enabled { 4 } else { 0 },
            flags: 0,
            color: 0xA020_2020,
            animation: 0,
        };
        let data = CompositionAttribute {
            attribute: 19,
            data: (&raw mut accent).cast(),
            size: std::mem::size_of::<AccentPolicy>(),
        };
        return unsafe { set(hwnd, &data) }.as_bool();
    }
    false
}

fn set_dwm_value<T>(
    hwnd: HWND,
    attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE,
    value: &T,
) -> windows::core::Result<()> {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            (value as *const T).cast(),
            std::mem::size_of_val(value) as u32,
        )
    }
}

#[cfg_attr(test, derive(Default))]
pub struct WindowFrame {
    hwnd: HWND,
    state: Rc<FrameState>,
}

#[derive(Default)]
struct FrameState {
    caption: Cell<RECT>,
}

const SYSTEM_CAPTION_STYLES: u32 = WS_CAPTION.0 | WS_SYSMENU.0;
const SYSTEM_EDGE_STYLES: u32 =
    WS_EX_CLIENTEDGE.0 | WS_EX_DLGMODALFRAME.0 | WS_EX_STATICEDGE.0 | WS_EX_WINDOWEDGE.0;

impl WindowFrame {
    pub fn attach(window: &impl HasWindowHandle) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        // DWM applies its native radius and keeps maximized windows square.
        // Older Windows versions can reject the attribute without affecting the frame.
        let _ = set_dwm_value(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
        let _ = set_dwm_value(hwnd, DWMWA_BORDER_COLOR, &DWMWA_COLOR_NONE);
        let state = Rc::new(FrameState::default());
        // The subclass owns one reference until its window is destroyed.
        let data = Rc::into_raw(Rc::clone(&state));
        let installed = unsafe { SetWindowSubclass(hwnd, Some(hit_test), 0, data as usize) };
        if !installed.as_bool() {
            unsafe { drop(Rc::from_raw(data)) };
            return None;
        }
        // Winit retains standard caption bits on undecorated windows. Acrylic
        // can reveal what those bits let Windows paint into the redirection
        // surface, so use the same frameless HWND structure in both themes.
        unsafe {
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let extra = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            SetWindowLongW(hwnd, GWL_STYLE, frame_style(style) as i32);
            SetWindowLongW(hwnd, GWL_EXSTYLE, (extra & !SYSTEM_EDGE_STYLES) as i32);
            let _ = SetWindowPos(
                hwnd, None, 0, 0, 0, 0,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            );
        }
        Some(Self { hwnd, state })
    }

    pub fn enable_acrylic(&self) -> Result<&'static str, String> {
        enable_acrylic(self.hwnd).inspect_err(|_| self.disable_acrylic())
    }

    pub fn disable_acrylic(&self) {
        set_accent(self.hwnd, false);
        let _ = set_dwm_value(self.hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
        let _ = set_dwm_value(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(false));
        let _ = unsafe { DwmExtendFrameIntoClientArea(self.hwnd, &MARGINS::default()) };
    }

    pub fn set_caption(&self, rect: egui::Rect, pixels_per_point: f32) {
        self.state.caption.set(RECT {
            left: (rect.left() * pixels_per_point).ceil() as i32,
            top: (rect.top() * pixels_per_point).ceil() as i32,
            right: (rect.right() * pixels_per_point).floor() as i32,
            bottom: (rect.bottom() * pixels_per_point).floor() as i32,
        });
    }
}

fn frame_style(style: u32) -> u32 {
    (style & !SYSTEM_CAPTION_STYLES) | WS_POPUP.0
}

unsafe extern "system" fn hit_test(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    let state = data as *const FrameState;
    if message == WM_STYLECHANGING && lparam.0 != 0 {
        // Winit can reapply its default styles when showing or maximizing.
        let style = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        match wparam.0 as i32 {
            index if index == GWL_STYLE.0 => style.styleNew = frame_style(style.styleNew),
            index if index == GWL_EXSTYLE.0 => style.styleNew &= !SYSTEM_EDGE_STYLES,
            _ => {}
        }
    } else if message == WM_NCCALCSIZE {
        if lparam.0 != 0 && unsafe { IsZoomed(window) }.as_bool() {
            // Both forms of WM_NCCALCSIZE begin with the proposed outer RECT.
            let rect = lparam.0 as *mut RECT;
            let monitor = unsafe { MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST) };
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                unsafe { *rect = info.rcWork };
            }
        }
        return LRESULT(0);
    } else if message == WM_NCPAINT {
        return LRESULT(0);
    } else if message == WM_NCACTIVATE {
        return unsafe { DefWindowProcW(window, message, wparam, LPARAM(-1)) };
    } else if message == WM_NCHITTEST {
        // Both coordinates are signed because a monitor can be left of or above the primary.
        let mut point = POINT {
            x: i32::from(lparam.0 as i16),
            y: i32::from((lparam.0 >> 16) as i16),
        };
        let mut bounds = RECT::default();
        if !unsafe { IsZoomed(window) }.as_bool()
            && unsafe { GetWindowRect(window, &mut bounds) }.is_ok()
        {
            let dpi = unsafe { GetDpiForWindow(window) };
            let padding = unsafe { GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
            let horizontal = unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) } + padding;
            let vertical = unsafe { GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) } + padding;
            if let Some(edge) = resize_edge(bounds, point, horizontal, vertical) {
                return LRESULT(edge as isize);
            }
        }
        if unsafe { ScreenToClient(window, &mut point) }.as_bool()
            && contains(unsafe { (*state).caption.get() }, point)
        {
            return LRESULT(HTCAPTION as isize);
        }
    } else if message == WM_NCDESTROY
        && unsafe { RemoveWindowSubclass(window, Some(hit_test), id) }.as_bool()
    {
        unsafe { drop(Rc::from_raw(state)) };
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

fn contains(rect: RECT, point: POINT) -> bool {
    (rect.left..rect.right).contains(&point.x) && (rect.top..rect.bottom).contains(&point.y)
}

fn resize_edge(bounds: RECT, point: POINT, horizontal: i32, vertical: i32) -> Option<u32> {
    if !contains(bounds, point) {
        return None;
    }
    let column = i32::from(point.x >= bounds.right - horizontal)
        - i32::from(point.x < bounds.left + horizontal);
    let row =
        i32::from(point.y >= bounds.bottom - vertical) - i32::from(point.y < bounds.top + vertical);
    match (column, row) {
        (-1, -1) => Some(HTTOPLEFT),
        (0, -1) => Some(HTTOP),
        (1, -1) => Some(HTTOPRIGHT),
        (-1, 0) => Some(HTLEFT),
        (1, 0) => Some(HTRIGHT),
        (-1, 1) => Some(HTBOTTOMLEFT),
        (0, 1) => Some(HTBOTTOM),
        (1, 1) => Some(HTBOTTOMRIGHT),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_keeps_all_edges_and_corners_on_scaled_negative_origin_displays() {
        let bounds = RECT {
            left: -1800,
            top: -600,
            right: -720,
            bottom: 80,
        };
        for thickness in [8, 10, 12] {
            let left = bounds.left + thickness - 1;
            let right = bounds.right - thickness;
            let top = bounds.top + thickness - 1;
            let bottom = bounds.bottom - thickness;
            let center_x = (bounds.left + bounds.right) / 2;
            let center_y = (bounds.top + bounds.bottom) / 2;
            for (x, y, expected) in [
                (left, top, Some(HTTOPLEFT)),
                (center_x, top, Some(HTTOP)),
                (right, top, Some(HTTOPRIGHT)),
                (left, center_y, Some(HTLEFT)),
                (right, center_y, Some(HTRIGHT)),
                (left, bottom, Some(HTBOTTOMLEFT)),
                (center_x, bottom, Some(HTBOTTOM)),
                (right, bottom, Some(HTBOTTOMRIGHT)),
                (left + 1, top + 1, None),
                (right - 1, bottom - 1, None),
                (bounds.right, bottom, None),
            ] {
                assert_eq!(
                    resize_edge(bounds, POINT { x, y }, thickness, thickness),
                    expected
                );
            }
        }
    }
}
