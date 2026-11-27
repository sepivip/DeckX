//! Uses the existing DeckX window and event loop for the Windows notification area.

use std::{cell::Cell, ptr::null_mut};

use eframe::egui::{Context, ViewportId};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::w;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    UI::{
        Shell::{
            DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
            NIM_MODIFY, NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4,
            NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, RemoveWindowSubclass, SetWindowSubclass,
            Shell_NotifyIconGetRect, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, ICON_BIG, ICON_SMALL,
            IDI_APPLICATION, KillTimer, LoadIconW, MF_STRING, PostMessageW, RegisterWindowMessageW,
            SC_MINIMIZE, SW_HIDE, SW_SHOW, SendMessageW, SetForegroundWindow, SetMenuDefaultItem,
            SetTimer, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
            WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_GETICON, WM_LBUTTONUP, WM_NCDESTROY, WM_NULL,
            WM_PAINT, WM_SHOWWINDOW, WM_SYSCOMMAND, WM_TIMER,
        },
    },
};

const TRAY_MESSAGE: u32 = WM_APP + 37;
const WAKE_TIMER: usize = 0x4443;
const WAKE_INTERVAL_MS: u32 = 50;
const ID: u32 = 1;
const OPEN: u32 = 1;
const QUIT: u32 = 2;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

pub(crate) struct Tray {
    state: Box<State>,
}

struct State {
    window: Cell<HWND>,
    context: Context,
    enabled: Cell<bool>,
    hidden: Cell<bool>,
    connected: Cell<bool>,
    taskbar_created: u32,
}

impl Tray {
    /// The window and subclass both belong to the UI thread. The boxed state stays
    /// at the same address until the subclass is removed.
    pub(crate) fn attach(context: &eframe::CreationContext<'_>) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = context.window_handle().ok()?.as_raw() else {
            return None;
        };
        let window = handle.hwnd.get() as HWND;
        let state = Box::new(State {
            window: Cell::new(window),
            context: context.egui_ctx.clone(),
            enabled: Cell::default(),
            hidden: Cell::default(),
            connected: Cell::default(),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated").as_ptr()) },
        });
        let data = &*state as *const State as usize;
        let installed = unsafe { SetWindowSubclass(window, Some(receive), ID as usize, data) };
        (installed != 0).then(|| Self { state })
    }

    pub(crate) fn update(&self, enabled: bool, connected: bool) {
        let state = &self.state;
        state.enabled.set(enabled);
        let connection_changed = state.connected.replace(connected) != connected;
        if state.hidden.get() {
            if !enabled {
                state.show();
            } else if connection_changed {
                let mut data = state.data();
                data.uFlags = NIF_TIP | NIF_SHOWTIP;
                data.szTip = state.tip();
                unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
            }
        }
    }

    pub(crate) fn is_hidden(&self) -> bool {
        self.state.hidden.get()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let window = self.state.window.get();
        if !window.is_null() {
            self.state.remove_icon();
            unsafe { RemoveWindowSubclass(window, Some(receive), ID as usize) };
        }
    }
}

impl State {
    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.window.get(),
            uID: ID,
            ..Default::default()
        }
    }

    fn tip(&self) -> [u16; 128] {
        let text = if self.connected.get() {
            "DeckX · Deck connected"
        } else {
            "DeckX · Deck disconnected"
        };
        let mut buffer = [0; 128];
        for (slot, character) in buffer.iter_mut().zip(text.encode_utf16()) {
            *slot = character;
        }
        buffer
    }

    fn icon(&self) -> HICON {
        let window = self.window.get();
        for size in [ICON_SMALL, ICON_BIG] {
            let icon = unsafe { SendMessageW(window, WM_GETICON, size as usize, 0) as HICON };
            if !icon.is_null() {
                return icon;
            }
        }
        unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
    }

    fn add_icon(&self) -> bool {
        let mut data = self.data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        data.hIcon = self.icon();
        data.szTip = self.tip();
        unsafe {
            if data.hIcon.is_null() || Shell_NotifyIconW(NIM_ADD, &data) == 0 {
                return false;
            }
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            if Shell_NotifyIconW(NIM_SETVERSION, &data) == 0 {
                Shell_NotifyIconW(NIM_DELETE, &data);
                return false;
            }
        }
        true
    }

    fn remove_icon(&self) {
        if self.hidden.replace(false) {
            unsafe {
                KillTimer(self.window.get(), WAKE_TIMER);
                Shell_NotifyIconW(NIM_DELETE, &self.data());
            }
        }
    }

    fn hide(&self) -> bool {
        if self.hidden.get() {
            return true;
        }
        if !self.add_icon() {
            return false;
        }
        let window = self.window.get();
        if unsafe { SetTimer(window, WAKE_TIMER, WAKE_INTERVAL_MS, None) } == 0 {
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) };
            return false;
        }
        self.hidden.set(true);
        unsafe { ShowWindow(window, SW_HIDE) };
        true
    }

    fn show(&self) {
        self.remove_icon();
        let window = self.window.get();
        unsafe {
            ShowWindow(window, SW_SHOW);
            SetForegroundWindow(window);
        }
    }

    fn menu(&self) {
        let window = self.window.get();
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: window,
            uID: ID,
            ..Default::default()
        };
        let command = unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            AppendMenuW(menu, MF_STRING, OPEN as usize, w!("Open DeckX").as_ptr());
            AppendMenuW(menu, MF_STRING, QUIT as usize, w!("Quit DeckX").as_ptr());
            SetMenuDefaultItem(menu, OPEN, 0);
            let mut rect = RECT::default();
            let mut point = POINT::default();
            // The icon bounds also work when the context menu was opened with the keyboard.
            if Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0 {
                point.x = rect.left;
                point.y = rect.top;
            } else {
                GetCursorPos(&mut point);
            }
            SetForegroundWindow(window);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                window,
                null_mut(),
            );
            DestroyMenu(menu);
            PostMessageW(window, WM_NULL, 0, 0);
            command as u32
        };
        match command {
            OPEN => self.show(),
            QUIT => {
                unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
            }
            _ => {
                unsafe { Shell_NotifyIconW(NIM_SETFOCUS, &self.data()) };
            }
        }
    }
}

unsafe extern "system" fn receive(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // The subclass reference is valid as long as the Tray owns its boxed state.
    let state = unsafe { &*(data as *const State) };
    match message {
        WM_SYSCOMMAND
            if wparam as u32 & 0xfff0 == SC_MINIMIZE && state.enabled.get() && state.hide() =>
        {
            return 0;
        }
        WM_TIMER if wparam == WAKE_TIMER && state.hidden.get() => {
            // Windows suppresses winit's normal redraws for hidden windows. Deliver
            // pending egui work so deck events and local commands still get handled.
            if state.context.has_requested_repaint_for(&ViewportId::ROOT) {
                unsafe { PostMessageW(window, WM_PAINT, 0, 0) };
            }
            return 0;
        }
        WM_CLOSE if state.hidden.get() => {
            let result = unsafe { DefSubclassProc(window, message, wparam, lparam) };
            unsafe { PostMessageW(window, WM_PAINT, 0, 0) };
            return result;
        }
        TRAY_MESSAGE if state.hidden.get() && (lparam as u32 >> 16) == ID => {
            match lparam as u16 as u32 {
                NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => state.show(),
                WM_CONTEXTMENU => state.menu(),
                _ => {}
            }
            return 0;
        }
        WM_SHOWWINDOW if wparam != 0 => state.remove_icon(),
        WM_NCDESTROY => {
            state.remove_icon();
            state.window.set(null_mut());
            unsafe { RemoveWindowSubclass(window, Some(receive), id) };
        }
        message if message == state.taskbar_created && state.hidden.get() => {
            if !state.add_icon() {
                state.show();
            }
            return 0;
        }
        _ => {}
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}
