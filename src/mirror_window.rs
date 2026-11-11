//! Hosts the unmodified scrcpy renderer as a native Windows child surface.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT, SetLastError};
use windows_sys::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, DeleteObject, GetWindowRgnBox, MapWindowPoints, SetWindowRgn,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub struct EmbeddedWindow {
    window: HWND,
    visible: bool,
}

impl Default for EmbeddedWindow {
    fn default() -> Self {
        Self {
            window: null_mut(),
            visible: false,
        }
    }
}

fn bounded_surface(bounds: [i32; 4], client: RECT) -> [i32; 4] {
    let [x, y, width, height] = bounds;
    let x = x.clamp(0, client.right.max(0));
    let y = y.clamp(0, client.bottom.max(0));
    [
        x,
        y,
        width.clamp(0, (client.right - x).max(0)),
        height.clamp(0, (client.bottom - y).max(0)),
    ]
}

unsafe fn surface_bounds(window: HWND, parent: HWND) -> Result<[i32; 4], String> {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(window, &mut rect) } == 0 {
        return Err(format!(
            "无法读取投屏区域：{}",
            std::io::Error::last_os_error()
        ));
    }
    let mut origin = POINT {
        x: rect.left,
        y: rect.top,
    };
    unsafe { MapWindowPoints(null_mut(), parent, &mut origin, 1) };
    Ok([
        origin.x,
        origin.y,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ])
}

struct WindowSearch {
    pid: u32,
    window: HWND,
}

unsafe extern "system" fn find_window(window: HWND, parameter: LPARAM) -> i32 {
    // EnumWindows invokes this callback synchronously with our live stack value.
    let search = unsafe { &mut *(parameter as *mut WindowSearch) };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == search.pid {
        let mut title = [0_u16; 128];
        let length = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
        if String::from_utf16_lossy(&title[..length as usize]) == "AndroidTool - scrcpy" {
            search.window = window;
            return 0;
        }
    }
    1
}

#[cfg(windows)]
pub fn own_gamepad_overlay(parent: HWND) {
    unsafe extern "system" fn find(hwnd: HWND, parameter: LPARAM) -> i32 {
        let parent = parameter as HWND;
        let mut parent_pid = 0;
        let mut window_pid = 0;
        unsafe {
            GetWindowThreadProcessId(parent, &mut parent_pid);
            GetWindowThreadProcessId(hwnd, &mut window_pid);
        }
        if parent_pid != window_pid {
            return 1;
        }
        let mut title = [0_u16; 128];
        let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if String::from_utf16_lossy(&title[..length as usize]) == "AndroidTool - Floating Gamepad" {
            unsafe {
                // Owned popups stay above this app's child video, without global topmost.
                if GetWindow(hwnd, GW_OWNER) != parent {
                    SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, parent as isize);
                }
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                if style & WS_EX_LAYERED == 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | WS_EX_LAYERED) as isize);
                    SetLayeredWindowAttributes(hwnd, 0, 190, LWA_ALPHA);
                }
                let mut bounds: RECT = std::mem::zeroed();
                let mut region_bounds: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut bounds) != 0 {
                    let width = bounds.right - bounds.left;
                    let height = bounds.bottom - bounds.top;
                    if GetWindowRgnBox(hwnd, &mut region_bounds) == 0
                        || region_bounds.right != width
                        || region_bounds.bottom != height
                    {
                        let diameter =
                            (28 * windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) as i32 / 96)
                                .max(1);
                        let region = CreateRoundRectRgn(0, 0, width, height, diameter, diameter);
                        if !region.is_null() && SetWindowRgn(hwnd, region, 1) == 0 {
                            DeleteObject(region);
                        }
                    }
                }
            }
            0
        } else {
            1
        }
    }
    unsafe {
        EnumWindows(Some(find), parent as LPARAM);
    }
}

impl EmbeddedWindow {
    pub fn sync(
        &mut self,
        pid: u32,
        parent: HWND,
        bounds: [i32; 4],
        visible: bool,
    ) -> Result<bool, String> {
        // Handles belong to the live eframe window and the session's child process.
        unsafe {
            if IsWindow(parent) == 0 {
                return Err("AndroidTool 主窗口句柄无效，请切换独立窗口模式。".to_owned());
            }
            if self.window.is_null() {
                let mut search = WindowSearch {
                    pid,
                    window: null_mut(),
                };
                EnumWindows(Some(find_window), &mut search as *mut _ as LPARAM);
                if search.window.is_null() {
                    return Ok(false);
                }
                let window = search.window;
                ShowWindow(window, SW_HIDE);
                let original = GetWindowLongPtrW(window, GWL_STYLE);
                let style = (original as u32
                    & !(WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_VISIBLE))
                    | WS_CHILD
                    | WS_CLIPSIBLINGS;
                SetWindowLongPtrW(window, GWL_STYLE, style as isize);
                SetLastError(0);
                if SetParent(window, parent).is_null() {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(0) {
                        SetWindowLongPtrW(window, GWL_STYLE, original);
                        return Err(format!("内嵌 scrcpy 失败：{error}，可切换独立窗口模式。"));
                    }
                }
                // The main OpenGL surface must not paint over the native video surface.
                let parent_style = GetWindowLongPtrW(parent, GWL_STYLE);
                SetWindowLongPtrW(parent, GWL_STYLE, parent_style | WS_CLIPCHILDREN as isize);
                let extended = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
                SetWindowLongPtrW(
                    window,
                    GWL_EXSTYLE,
                    ((extended & !WS_EX_APPWINDOW) | WS_EX_TOOLWINDOW) as isize,
                );
                self.window = window;
            }
            if IsWindow(self.window) == 0 {
                return Err("scrcpy 视频窗口已关闭，请重新启动投屏。".to_owned());
            }
            let mut client: RECT = std::mem::zeroed();
            if GetClientRect(parent, &mut client) == 0 {
                return Err("无法读取主窗口可视区域。".to_owned());
            }
            let bounds = bounded_surface(bounds, client);
            let visible = visible && bounds[2] > 0 && bounds[3] > 0;
            // scrcpy resizes/repositions itself on first frame and orientation changes.
            // Compare the actual native rectangle every tick, not our last request.
            if surface_bounds(self.window, parent)? != bounds {
                let [x, y, width, height] = bounds;
                if SetWindowPos(
                    self.window,
                    null_mut(),
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
                ) == 0
                {
                    return Err(format!(
                        "无法调整投屏区域：{}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
            if self.visible != visible || (IsWindowVisible(self.window) != 0) != visible {
                ShowWindow(self.window, if visible { SW_SHOWNA } else { SW_HIDE });
                self.visible = visible;
            }
        }
        Ok(true)
    }

    pub fn is_attached(&self) -> bool {
        !self.window.is_null()
    }

    #[cfg(test)]
    pub fn verify(&self, parent: HWND, bounds: [i32; 4], visible: bool) {
        unsafe {
            assert_eq!(GetParent(self.window), parent);
            assert_ne!(
                GetWindowLongPtrW(self.window, GWL_STYLE) as u32 & WS_CHILD,
                0
            );
            let mut rect: RECT = std::mem::zeroed();
            assert_ne!(GetClientRect(self.window, &mut rect), 0);
            assert_eq!([rect.right, rect.bottom], [bounds[2], bounds[3]]);
            assert_eq!(surface_bounds(self.window, parent).unwrap(), bounds);
            assert_eq!(IsWindowVisible(self.window) != 0, visible);
        }
    }

    #[cfg(test)]
    pub fn simulate_scrcpy_resize(&self, bounds: [i32; 4]) {
        let [x, y, width, height] = bounds;
        unsafe {
            assert_ne!(
                SetWindowPos(
                    self.window,
                    null_mut(),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER
                ),
                0
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_gamepad_is_owned_by_host_not_globally_topmost() {
        unsafe {
            let class: Vec<_> = "STATIC\0".encode_utf16().collect();
            let title: Vec<_> = "AndroidTool - Floating Gamepad\0".encode_utf16().collect();
            let parent = CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                800,
                600,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            let overlay = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                330,
                246,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            assert!(!parent.is_null() && !overlay.is_null());
            own_gamepad_overlay(parent);
            let owner = GetWindow(overlay, GW_OWNER);
            let topmost = GetWindowLongPtrW(overlay, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST;
            let layered = GetWindowLongPtrW(overlay, GWL_EXSTYLE) as u32 & WS_EX_LAYERED;
            let mut alpha = 0;
            let mut flags = 0;
            let attributes =
                GetLayeredWindowAttributes(overlay, null_mut(), &mut alpha, &mut flags);
            let mut bounds: RECT = std::mem::zeroed();
            let region_kind = GetWindowRgnBox(overlay, &mut bounds);
            DestroyWindow(overlay);
            DestroyWindow(parent);
            assert_eq!(owner, parent);
            assert_eq!(topmost, 0);
            assert_ne!(layered, 0);
            assert_ne!(attributes, 0);
            assert_eq!(alpha, 190);
            assert_eq!(flags, LWA_ALPHA);
            assert_eq!(
                region_kind,
                windows_sys::Win32::Graphics::Gdi::COMPLEXREGION
            );
        }
    }

    #[test]
    fn video_surface_stays_in_parent_client_area() {
        let client = RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        };
        assert_eq!(
            bounded_surface([250, 360, 1400, 800], client),
            [250, 360, 1030, 360]
        );
        assert_eq!(
            bounded_surface([250, 800, 500, 600], client),
            [250, 720, 500, 0]
        );
        assert_eq!(
            bounded_surface([250, 100, 500, 600], client),
            [250, 100, 500, 600]
        );
    }
}
