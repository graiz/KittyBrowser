use super::input::{Button, Event, Mouse, MouseKind, Parser};
use crate::platform::windows::wide;
use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::Threading::*;

static ORIGINAL: Mutex<Option<(u32, u32)>> = Mutex::new(None);

fn raw_input_mode(input: u32) -> u32 {
    // PSReadLine can leave mouse input disabled when it starts a command.
    (input | ENABLE_MOUSE_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_EXTENDED_FLAGS)
        & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_QUICK_EDIT_MODE)
}

pub fn enter_raw() -> io::Result<()> {
    let (mut input, mut output) = (0, 0);
    unsafe {
        let stdin = GetStdHandle(STD_INPUT_HANDLE);
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if GetConsoleMode(stdin, &mut input) == 0 || GetConsoleMode(stdout, &mut output) == 0 {
            return Err(io::Error::other("needs to run in a Windows terminal"));
        }
        let mut saved = ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
        saved.get_or_insert((input, output));
        let raw = raw_input_mode(input);
        if SetConsoleMode(stdout, output | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
            return Err(io::Error::last_os_error());
        }
        if SetConsoleMode(stdin, raw) == 0 {
            let error = io::Error::last_os_error();
            SetConsoleMode(stdout, output);
            return Err(error);
        }
    }
    Ok(())
}

pub fn restore() {
    if let Some((input, output)) = *ORIGINAL.lock().unwrap_or_else(|e| e.into_inner()) {
        unsafe {
            SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), input);
            SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), output);
        }
    }
}

thread_local! {
    static PENDING: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static SURROGATE: std::cell::Cell<Option<u16>> = const { std::cell::Cell::new(None) };
    static MOUSE: std::cell::RefCell<MouseState> = const { std::cell::RefCell::new(MouseState {
        buttons: 0, wheel_x: 0, wheel_y: 0,
    }) };
}

pub fn read_input(buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
    let drain = |pending: &mut Vec<u8>| {
        let n = pending.len().min(buf.len());
        buf[..n].copy_from_slice(&pending[..n]);
        pending.drain(..n);
        n
    };
    let mut drain = drain;
    let n = PENDING.with(|pending| drain(&mut pending.borrow_mut()));
    if n > 0 {
        return Ok(Some(n));
    }
    let Some(records) = read_records(timeout)? else { return Ok(None) };
    PENDING.with(|pending| {
        let mut bytes = pending.borrow_mut();
        for record in records {
            if record.EventType as u32 == KEY_EVENT {
                append_key(unsafe { record.Event.KeyEvent }, &mut bytes);
            }
        }
        let n = drain(&mut bytes);
        Ok((n > 0).then_some(n))
    })
}

fn read_records(timeout: Duration) -> io::Result<Option<Vec<INPUT_RECORD>>> {
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    // Console modes are shared with the hosting shell and other attached clients.
    let mut mode = 0;
    if unsafe { GetConsoleMode(input, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = raw_input_mode(mode);
    if raw != mode && unsafe { SetConsoleMode(input, raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ready = unsafe { WaitForSingleObject(input, timeout.as_millis().min(u32::MAX as u128 - 1) as u32) };
    if ready == WAIT_TIMEOUT {
        return Ok(None);
    }
    if ready != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    let mut records: [INPUT_RECORD; 64] = unsafe { std::mem::zeroed() };
    let mut count = 0;
    if unsafe { ReadConsoleInputW(input, records.as_mut_ptr(), records.len() as u32, &mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(records[..count as usize].to_vec()))
}

fn append_key(key: KEY_EVENT_RECORD, bytes: &mut Vec<u8>) {
    if key.bKeyDown == 0 {
        return;
    }
    let unit = unsafe { key.uChar.UnicodeChar };
    if unit == 0 {
        return;
    }
    for _ in 0..key.wRepeatCount {
        SURROGATE.with(|saved| {
            if (0xd800..=0xdbff).contains(&unit) {
                saved.set(Some(unit));
                return;
            }
            let code = match saved.take() {
                Some(high) if (0xdc00..=0xdfff).contains(&unit) => {
                    0x10000 + ((high as u32 - 0xd800) << 10) + unit as u32 - 0xdc00
                }
                _ => unit as u32,
            };
            let character = char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER);
            bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        });
    }
}

pub fn read_events(parser: &mut Parser, timeout: Duration) -> io::Result<Option<Vec<Event>>> {
    let Some(records) = read_records(timeout)? else { return Ok(None) };
    let mut bytes = Vec::new();
    let mut events = Vec::new();
    for record in records {
        match record.EventType as u32 {
            KEY_EVENT => append_key(unsafe { record.Event.KeyEvent }, &mut bytes),
            MOUSE_EVENT => {
                events.extend(parser.feed(&bytes));
                bytes.clear();
                let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
                if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                MOUSE.with(|state| {
                    events.extend(state.borrow_mut().feed(unsafe { record.Event.MouseEvent }, info.srWindow));
                });
            }
            _ => {}
        }
    }
    events.extend(parser.feed(&bytes));
    Ok(Some(events))
}

#[derive(Default)]
struct MouseState {
    buttons: u32,
    wheel_x: i32,
    wheel_y: i32,
}

impl MouseState {
    fn feed(&mut self, record: MOUSE_EVENT_RECORD, viewport: SMALL_RECT) -> Vec<Event> {
        let buttons = record.dwButtonState & 31;
        let base = Mouse {
            kind: MouseKind::Move,
            button: None,
            x: (i32::from(record.dwMousePosition.X) - i32::from(viewport.Left) + 1).max(1) as u32,
            y: (i32::from(record.dwMousePosition.Y) - i32::from(viewport.Top) + 1).max(1) as u32,
            cell_coordinates: true,
            ctrl: record.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0,
            alt: record.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0,
            shift: record.dwControlKeyState & SHIFT_PRESSED != 0,
        };
        let mut events = Vec::new();
        for (bit, button) in [
            (FROM_LEFT_1ST_BUTTON_PRESSED, Button::Left),
            (FROM_LEFT_2ND_BUTTON_PRESSED, Button::Middle),
            (RIGHTMOST_BUTTON_PRESSED, Button::Right),
            (FROM_LEFT_3RD_BUTTON_PRESSED, Button::Back),
            (FROM_LEFT_4TH_BUTTON_PRESSED, Button::Forward),
        ] {
            if (buttons ^ self.buttons) & bit != 0 {
                let kind = if buttons & bit != 0 { MouseKind::Press } else { MouseKind::Release };
                events.push(Event::Mouse(Mouse { kind, button: Some(button), ..base }));
            }
        }
        self.buttons = buttons;
        if record.dwEventFlags & MOUSE_MOVED != 0 {
            let button = match buttons {
                b if b & FROM_LEFT_1ST_BUTTON_PRESSED != 0 => Some(Button::Left),
                b if b & FROM_LEFT_2ND_BUTTON_PRESSED != 0 => Some(Button::Middle),
                b if b & RIGHTMOST_BUTTON_PRESSED != 0 => Some(Button::Right),
                _ => None,
            };
            events.push(Event::Mouse(Mouse { button, ..base }));
        }
        let delta = (record.dwButtonState >> 16) as i16 as i32;
        let wheel = if record.dwEventFlags & MOUSE_WHEELED != 0 {
            self.wheel_y += delta;
            let ticks = self.wheel_y / 120;
            self.wheel_y %= 120;
            Some((0, -ticks))
        } else if record.dwEventFlags & MOUSE_HWHEELED != 0 {
            self.wheel_x += delta;
            let ticks = self.wheel_x / 120;
            self.wheel_x %= 120;
            Some((ticks, 0))
        } else {
            None
        };
        if let Some((x, y)) = wheel.filter(|ticks| *ticks != (0, 0)) {
            events.push(Event::Mouse(Mouse { kind: MouseKind::Wheel(x, y), ..base }));
        }
        events
    }
}

pub fn draws_graphics(limit: Duration) -> Option<bool> {
    enter_raw().ok()?;
    super::write(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
    let mut answer = Vec::new();
    let deadline = Instant::now() + limit;
    let result = loop {
        if let Some(kind) = super::graphics_kind(&answer) {
            if let Some(kind) = kind {
                super::set_graphics(kind);
            }
            break Some(kind.is_some());
        }
        if Instant::now() >= deadline {
            break None;
        }
        let mut buf = [0u8; 512];
        match read_input(&mut buf, Duration::from_millis(50)) {
            Ok(Some(n)) => answer.extend_from_slice(&buf[..n]),
            Ok(None) => {}
            Err(_) => break None,
        }
    };
    restore();
    result
}

struct ClipboardWindow(HWND);

impl ClipboardWindow {
    fn new() -> io::Result<Self> {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let window = unsafe {
            CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(window))
    }
}

impl Drop for ClipboardWindow {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0) };
    }
}

pub fn copy(text: &str) -> io::Result<()> {
    use windows_sys::Win32::System::DataExchange::*;
    use windows_sys::Win32::System::Memory::*;
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
    // EmptyClipboard needs a window owner for the subsequent SetClipboardData.
    let window = ClipboardWindow::new()?;
    let value = wide(text);
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, value.len() * 2) };
    if memory.is_null() {
        return Err(io::Error::last_os_error());
    }
    let pointer = unsafe { GlobalLock(memory) };
    if pointer.is_null() {
        unsafe { GlobalFree(memory) };
        return Err(io::Error::last_os_error());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(value.as_ptr(), pointer.cast(), value.len());
        GlobalUnlock(memory);
    }
    if unsafe { OpenClipboard(window.0) } == 0 {
        unsafe { GlobalFree(memory) };
        return Err(io::Error::last_os_error());
    }
    let ok = unsafe { EmptyClipboard() != 0 && !SetClipboardData(CF_UNICODETEXT as u32, memory).is_null() };
    let error = io::Error::last_os_error();
    unsafe { CloseClipboard() };
    if !ok {
        unsafe { GlobalFree(memory) };
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_repairs_console_modes_changed_by_another_client() {
        use std::os::windows::process::CommandExt;
        const CHILD: &str = "KB_CONSOLE_MODE_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "term::windows::tests::reader_repairs_console_modes_changed_by_another_client",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use windows_sys::Win32::Storage::FileSystem::*;
        // CREATE_NO_WINDOW gives this child a private console without touching the user's pane.
        unsafe {
            let handle = CreateFileW(
                wide("CONIN$").as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            assert_ne!(handle, INVALID_HANDLE_VALUE, "{}", io::Error::last_os_error());
            let original = GetStdHandle(STD_INPUT_HANDLE);
            assert_ne!(SetStdHandle(STD_INPUT_HANDLE, handle), 0);
            let mut mode = 0;
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            let cooked =
                (mode | ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_EXTENDED_FLAGS)
                    & !ENABLE_MOUSE_INPUT;
            assert_ne!(SetConsoleMode(handle, cooked), 0);
            read_records(Duration::ZERO).unwrap();
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            assert_eq!(mode, raw_input_mode(cooked));
            assert_ne!(SetStdHandle(STD_INPUT_HANDLE, original), 0);
            assert_ne!(CloseHandle(handle), 0);
        }
    }

    fn mouse_record(buttons: u32, flags: u32) -> MOUSE_EVENT_RECORD {
        MOUSE_EVENT_RECORD {
            dwMousePosition: COORD { X: 10, Y: 55 },
            dwButtonState: buttons,
            dwControlKeyState: LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED | SHIFT_PRESSED,
            dwEventFlags: flags,
        }
    }

    #[test]
    fn native_clicks_and_drags_use_pane_cells_and_preserve_modifiers() {
        let viewport = SMALL_RECT { Left: 4, Top: 50, Right: 80, Bottom: 70 };
        let mut state = MouseState::default();
        let down = state.feed(mouse_record(FROM_LEFT_1ST_BUTTON_PRESSED, 0), viewport);
        let Event::Mouse(mouse) = down[0] else { panic!("expected a mouse event") };
        assert_eq!(down.len(), 1);
        assert_eq!((mouse.x, mouse.y), (7, 6));
        assert!(mouse.cell_coordinates && mouse.ctrl && mouse.alt && mouse.shift);
        assert_eq!(mouse.kind, MouseKind::Press);
        assert_eq!(mouse.button, Some(Button::Left));
        assert_eq!(
            state.feed(mouse_record(FROM_LEFT_1ST_BUTTON_PRESSED, MOUSE_MOVED), viewport),
            [Event::Mouse(Mouse { kind: MouseKind::Move, ..mouse })]
        );
        assert_eq!(
            state.feed(mouse_record(0, 0), viewport),
            [Event::Mouse(Mouse { kind: MouseKind::Release, ..mouse })]
        );
        assert_eq!(
            state.feed(mouse_record(FROM_LEFT_1ST_BUTTON_PRESSED, DOUBLE_CLICK), viewport),
            [Event::Mouse(mouse)]
        );
    }

    #[test]
    fn native_wheels_accumulate_partial_ticks_with_the_right_direction() {
        let viewport = SMALL_RECT { Left: 0, Top: 0, Right: 80, Bottom: 70 };
        let mut state = MouseState::default();
        assert!(state.feed(mouse_record(60 << 16, MOUSE_WHEELED), viewport).is_empty());
        let Event::Mouse(up) = state.feed(mouse_record(60 << 16, MOUSE_WHEELED), viewport)[0] else {
            panic!("expected a wheel event")
        };
        assert_eq!(up.kind, MouseKind::Wheel(0, -1));
        let Event::Mouse(down) = state.feed(mouse_record((-120_i16 as u16 as u32) << 16, MOUSE_WHEELED), viewport)[0]
        else {
            panic!("expected a wheel event")
        };
        assert_eq!(down.kind, MouseKind::Wheel(0, 1));
        let Event::Mouse(right) = state.feed(mouse_record(120 << 16, MOUSE_HWHEELED), viewport)[0] else {
            panic!("expected a wheel event")
        };
        assert_eq!(right.kind, MouseKind::Wheel(1, 0));
    }

    #[test]
    fn raw_mode_enables_mouse_input_after_an_interactive_shell() {
        use super::*;
        let shell = ENABLE_ECHO_INPUT | ENABLE_QUICK_EDIT_MODE | ENABLE_INSERT_MODE | ENABLE_EXTENDED_FLAGS;
        let raw = raw_input_mode(shell);
        assert_ne!(raw & ENABLE_MOUSE_INPUT, 0);
        assert_ne!(raw & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
        assert_ne!(raw & ENABLE_EXTENDED_FLAGS, 0);
        assert_eq!(raw & (ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_QUICK_EDIT_MODE), 0);
        assert_ne!(raw & ENABLE_INSERT_MODE, 0);
        assert_eq!(raw_input_mode(shell | ENABLE_MOUSE_INPUT), raw);
    }

    #[test]
    fn unicode_clipboard_survives_its_owner_without_touching_the_users_clipboard() {
        const CHILD: &str = "KB_CLIPBOARD_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "term::windows::tests::unicode_clipboard_survives_its_owner_without_touching_the_users_clipboard",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use windows_sys::Win32::System::DataExchange::*;
        use windows_sys::Win32::System::Memory::*;
        use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
        use windows_sys::Win32::System::StationsAndDesktops::*;
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        // Clipboard data is scoped to a window station. Isolate the entire test.
        unsafe {
            let original_station = GetProcessWindowStation();
            let original_desktop = GetThreadDesktop(GetCurrentThreadId());
            let station = CreateWindowStationW(
                std::ptr::null(),
                0,
                windows_sys::Win32::Foundation::GENERIC_ALL,
                std::ptr::null(),
            );
            assert!(!station.is_null(), "{}", std::io::Error::last_os_error());
            assert_ne!(SetProcessWindowStation(station), 0);
            let desktop = CreateDesktopW(
                super::wide("kb-clipboard-test").as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                DESKTOP_CREATEWINDOW | DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS,
                std::ptr::null(),
            );
            assert!(!desktop.is_null(), "{}", std::io::Error::last_os_error());
            assert_ne!(SetThreadDesktop(desktop), 0);
            let text = "caf\u{e9}\u{1f43e}";
            super::copy(text).unwrap();
            assert_ne!(OpenClipboard(std::ptr::null_mut()), 0);
            let memory = GetClipboardData(CF_UNICODETEXT as u32);
            assert!(!memory.is_null());
            let pointer = GlobalLock(memory).cast::<u16>();
            assert!(!pointer.is_null());
            assert_eq!(std::slice::from_raw_parts(pointer, text.encode_utf16().count() + 1), super::wide(text));
            GlobalUnlock(memory);
            assert_ne!(CloseClipboard(), 0);
            assert_ne!(SetThreadDesktop(original_desktop), 0);
            assert_ne!(SetProcessWindowStation(original_station), 0);
            assert_ne!(CloseDesktop(desktop), 0);
            assert_ne!(CloseWindowStation(station), 0);
        }
    }
}
