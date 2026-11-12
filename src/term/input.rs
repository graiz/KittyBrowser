//! Decodes what the terminal sends: keys, mouse reports (SGR encoding, in
//! cells or pixels), pastes, and replies to our queries.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    fn plain(code: KeyCode) -> Key {
        Key { code, ctrl: false, alt: false, shift: false }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Press,
    Release,
    Move,
    /// Wheel ticks: (right, down).
    Wheel(i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mouse {
    pub kind: MouseKind,
    /// The button pressed or released, or held during a move.
    pub button: Option<Button>,
    /// 1-based, in cells or in pixels depending on the mode in effect.
    pub x: u32,
    pub y: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    Key(Key),
    Mouse(Mouse),
    Paste(String),
    /// Reply to `CSI 16 t`: one cell, in pixels.
    CellSize { width: u32, height: u32 },
    /// Reply to the mode 1016 query: whether mouse reports are in pixels.
    PixelMouse(bool),
}

const PASTE_END: &[u8] = b"\x1b[201~";

/// Incremental decoder. Bytes go in as they arrive; a sequence split across
/// reads is held until the rest shows up.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    pasting: bool,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut pos = 0;
        while pos < self.buf.len() {
            match self.next(pos, &mut events) {
                Some(used) => pos += used,
                None => break,
            }
        }
        self.buf.drain(..pos);
        events
    }

    /// Decode one item at `pos`, returning how many bytes it used, or `None`
    /// if it is incomplete.
    fn next(&mut self, pos: usize, events: &mut Vec<Event>) -> Option<usize> {
        let rest = &self.buf[pos..];
        if self.pasting {
            let end = rest.windows(PASTE_END.len()).position(|w| w == PASTE_END)?;
            events.push(Event::Paste(String::from_utf8_lossy(&rest[..end]).into_owned()));
            self.pasting = false;
            return Some(end + PASTE_END.len());
        }
        if rest[0] != 0x1b {
            let (key, used) = plain_key(rest)?;
            events.extend(key.map(Event::Key));
            return Some(used);
        }
        match rest.get(1) {
            // Nothing follows in this read, so it's the Esc key itself.
            None | Some(0x1b) => {
                events.push(Event::Key(Key::plain(KeyCode::Esc)));
                Some(1)
            }
            Some(b'[') => {
                let end = 2 + rest[2..].iter().position(|b| (0x40..=0x7e).contains(b))?;
                let body = std::str::from_utf8(&rest[2..end]).unwrap_or("");
                if body == "200" && rest[end] == b'~' {
                    self.pasting = true;
                } else {
                    events.extend(csi(body, rest[end]));
                }
                Some(end + 1)
            }
            Some(b'O') => {
                let code = match rest.get(2)? {
                    b'A' => Some(KeyCode::Up),
                    b'B' => Some(KeyCode::Down),
                    b'C' => Some(KeyCode::Right),
                    b'D' => Some(KeyCode::Left),
                    b'H' => Some(KeyCode::Home),
                    b'F' => Some(KeyCode::End),
                    _ => None,
                };
                events.extend(code.map(|code| Event::Key(Key::plain(code))));
                Some(3)
            }
            // Esc then a key is how terminals send Alt. Option+arrows often
            // arrive as Esc b / Esc f, the readline word moves.
            Some(_) => {
                let (key, used) = plain_key(&rest[1..])?;
                events.extend(key.map(|key| {
                    let code = match key.code {
                        KeyCode::Char('b') if !key.ctrl => KeyCode::Left,
                        KeyCode::Char('f') if !key.ctrl => KeyCode::Right,
                        code => code,
                    };
                    Event::Key(Key { code, alt: true, ..key })
                }));
                Some(1 + used)
            }
        }
    }
}

/// A key that isn't an escape sequence: a control byte or one UTF-8
/// character. Returns the key (if the byte means anything) and bytes used.
fn plain_key(bytes: &[u8]) -> Option<(Option<Key>, usize)> {
    let first = bytes[0];
    let key = match first {
        b'\r' | b'\n' => Some(Key::plain(KeyCode::Enter)),
        b'\t' => Some(Key::plain(KeyCode::Tab)),
        0x7f | 0x08 => Some(Key::plain(KeyCode::Backspace)),
        0x01..=0x1a => {
            Some(Key { ctrl: true, ..Key::plain(KeyCode::Char((first - 1 + b'a') as char)) })
        }
        0x00..=0x1f => None,
        _ => {
            let len = match first {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => return Some((None, 1)),
            };
            let Ok(text) = std::str::from_utf8(bytes.get(..len)?) else {
                return Some((None, 1));
            };
            return Some((text.chars().next().map(|c| Key::plain(KeyCode::Char(c))), len));
        }
    };
    Some((key, 1))
}

/// Decode a CSI sequence given what lies between `ESC [` and the final byte.
fn csi(body: &str, last: u8) -> Option<Event> {
    if let Some(report) = body.strip_prefix('<') {
        return mouse(report, last == b'M');
    }
    if let Some(mode) = body.strip_prefix("?1016;") {
        // 1 and 3 mean set; 0, 2 and 4 mean unknown or reset.
        return (last == b'y').then_some(Event::PixelMouse(matches!(mode, "1$" | "3$")));
    }
    let mut numbers = body.split(';').map(|n| n.parse::<u32>().unwrap_or(0));
    let first = numbers.next().unwrap_or(0);
    let second = numbers.next().unwrap_or(0);
    let code = match last {
        b't' if first == 6 => {
            let width = numbers.next().filter(|w| *w > 0)?;
            return (second > 0).then_some(Event::CellSize { width, height: second });
        }
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => KeyCode::BackTab,
        b'~' => match first {
            1 | 7 => KeyCode::Home,
            4 | 8 => KeyCode::End,
            3 => KeyCode::Delete,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            _ => return None,
        },
        _ => return None,
    };
    // Modifiers are sent as 1 + a bit mask.
    let mods = second.saturating_sub(1);
    Some(Event::Key(Key { code, shift: mods & 1 != 0, alt: mods & 2 != 0, ctrl: mods & 4 != 0 }))
}

/// Decode an SGR mouse report, `button;x;y`.
fn mouse(report: &str, pressed: bool) -> Option<Event> {
    let mut numbers = report.split(';').map(|n| n.parse::<u32>().ok());
    let bits = numbers.next()??;
    let x = numbers.next()??;
    let y = numbers.next()??;
    let button = match bits & 3 {
        0 => Some(Button::Left),
        1 => Some(Button::Middle),
        2 => Some(Button::Right),
        _ => None,
    };
    let (kind, button) = if bits & 64 != 0 {
        let ticks = match bits & 3 {
            0 => (0, -1),
            1 => (0, 1),
            2 => (-1, 0),
            _ => (1, 0),
        };
        (MouseKind::Wheel(ticks.0, ticks.1), None)
    } else if bits & 32 != 0 {
        (MouseKind::Move, button)
    } else if pressed {
        (MouseKind::Press, button)
    } else {
        (MouseKind::Release, button)
    };
    Some(Event::Mouse(Mouse {
        kind,
        button,
        x,
        y,
        shift: bits & 4 != 0,
        alt: bits & 8 != 0,
        ctrl: bits & 16 != 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<Event> {
        Parser::default().feed(bytes)
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(Key::plain(code))
    }

    #[test]
    fn text_and_control_keys() {
        assert_eq!(
            parse("aé\r\x7f\t".as_bytes()),
            [
                key(KeyCode::Char('a')),
                key(KeyCode::Char('é')),
                key(KeyCode::Enter),
                key(KeyCode::Backspace),
                key(KeyCode::Tab),
            ]
        );
        let ctrl_l = Key { ctrl: true, ..Key::plain(KeyCode::Char('l')) };
        assert_eq!(parse(b"\x0c"), [Event::Key(ctrl_l)]);
    }

    #[test]
    fn arrows_and_modifiers() {
        assert_eq!(parse(b"\x1b[A\x1bOD\x1b[3~"), [key(KeyCode::Up), key(KeyCode::Left), key(KeyCode::Delete)]);
        let alt_left = Event::Key(Key { alt: true, ..Key::plain(KeyCode::Left) });
        assert_eq!(parse(b"\x1b[1;3D"), [alt_left.clone()]);
        assert_eq!(parse(b"\x1bb"), [alt_left]);
        let alt_x = Key { alt: true, ..Key::plain(KeyCode::Char('x')) };
        assert_eq!(parse(b"\x1bx"), [Event::Key(alt_x)]);
    }

    #[test]
    fn lone_escape_is_the_esc_key() {
        assert_eq!(parse(b"\x1b"), [key(KeyCode::Esc)]);
    }

    #[test]
    fn mouse_reports() {
        let base = Mouse { kind: MouseKind::Press, button: Some(Button::Left), x: 12, y: 3, ctrl: false, alt: false, shift: false };
        assert_eq!(parse(b"\x1b[<0;12;3M"), [Event::Mouse(base)]);
        assert_eq!(parse(b"\x1b[<0;12;3m"), [Event::Mouse(Mouse { kind: MouseKind::Release, ..base })]);
        assert_eq!(parse(b"\x1b[<35;12;3M"), [Event::Mouse(Mouse { kind: MouseKind::Move, button: None, ..base })]);
        assert_eq!(parse(b"\x1b[<32;12;3M"), [Event::Mouse(Mouse { kind: MouseKind::Move, ..base })]);
        assert_eq!(
            parse(b"\x1b[<65;12;3M"),
            [Event::Mouse(Mouse { kind: MouseKind::Wheel(0, 1), button: None, ..base })]
        );
        assert_eq!(
            parse(b"\x1b[<18;12;3M"),
            [Event::Mouse(Mouse { button: Some(Button::Right), ctrl: true, ..base })]
        );
    }

    #[test]
    fn sequences_split_across_reads() {
        let mut parser = Parser::default();
        assert_eq!(parser.feed(b"\x1b[<0;1"), []);
        assert_eq!(parser.feed(b"2;3M").len(), 1);
        assert_eq!(parser.feed(&"é".as_bytes()[..1]), []);
        assert_eq!(parser.feed(&"é".as_bytes()[1..]), [key(KeyCode::Char('é'))]);
    }

    #[test]
    fn paste_is_one_event() {
        let mut parser = Parser::default();
        assert_eq!(parser.feed(b"\x1b[200~hello\nwor"), []);
        assert_eq!(parser.feed(b"ld\x1b[201~x"), [Event::Paste("hello\nworld".into()), key(KeyCode::Char('x'))]);
    }

    #[test]
    fn query_replies() {
        assert_eq!(parse(b"\x1b[6;34;16t"), [Event::CellSize { width: 16, height: 34 }]);
        assert_eq!(parse(b"\x1b[?1016;1$y"), [Event::PixelMouse(true)]);
        assert_eq!(parse(b"\x1b[?1016;0$y"), [Event::PixelMouse(false)]);
        assert_eq!(parse(b"\x1b[4;600;800t"), []);
    }
}
