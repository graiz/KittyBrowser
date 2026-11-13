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
    /// A function key, F1 to F12.
    F(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Command on a Mac. Only terminals with the kitty keyboard protocol
    /// report it.
    pub cmd: bool,
}

impl Default for Key {
    fn default() -> Key {
        Key::plain(KeyCode::Esc)
    }
}

impl Key {
    fn plain(code: KeyCode) -> Key {
        Key { code, ctrl: false, alt: false, shift: false, cmd: false }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
    /// The extra buttons many mice have, which browsers use to go back and
    /// forward.
    Back,
    Forward,
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
    CellSize {
        width: u32,
        height: u32,
    },
    /// Reply to the mode 1016 query: whether mouse reports are in pixels.
    PixelMouse(bool),
    /// The pane gained or lost keyboard focus.
    Focus(bool),
    /// Reply to a graphics command: the image id it named, and whether the
    /// terminal accepted it.
    Graphics {
        id: u32,
        ok: bool,
    },
    /// Reply to an OSC 10 or 11 query: the terminal's text (10) or
    /// background (11) colour.
    Color {
        slot: u8,
        rgb: (u8, u8, u8),
    },
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
                    b'P' => Some(KeyCode::F(1)),
                    b'Q' => Some(KeyCode::F(2)),
                    b'R' => Some(KeyCode::F(3)),
                    b'S' => Some(KeyCode::F(4)),
                    _ => None,
                };
                events.extend(code.map(|code| Event::Key(Key::plain(code))));
                Some(3)
            }
            // A graphics reply: ESC _ G i=<id>;<OK or an error> ESC \
            Some(b'_') => {
                let end = 2 + rest[2..].windows(2).position(|w| w == b"\x1b\\")?;
                let body = std::str::from_utf8(&rest[2..end]).unwrap_or("");
                if let Some((keys, message)) = body.strip_prefix('G').and_then(|body| body.split_once(';')) {
                    let id = keys.split(',').find_map(|key| key.strip_prefix("i=")).and_then(|id| id.parse().ok());
                    events.extend(id.map(|id| Event::Graphics { id, ok: message == "OK" }));
                }
                Some(end + 2)
            }
            // A reply to a colour query: ESC ] 11 ; rgb:RRRR/GGGG/BBBB, ended
            // by BEL or ESC \. Replies start with a digit; anything else is
            // Alt+] followed by a key.
            Some(b']') if rest.get(2).is_some_and(u8::is_ascii_digit) || rest.len() == 2 => {
                let body_end = rest[2..].iter().position(|b| *b == 0x07 || *b == 0x1b)? + 2;
                let used = match rest[body_end] {
                    0x07 => body_end + 1,
                    _ if rest.get(body_end + 1) == Some(&b'\\') => body_end + 2,
                    _ if rest.len() == body_end + 1 => return None,
                    _ => body_end + 1,
                };
                events.extend(color_reply(std::str::from_utf8(&rest[2..body_end]).unwrap_or("")));
                Some(used)
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
        0x01..=0x1a => Some(Key { ctrl: true, ..Key::plain(KeyCode::Char((first - 1 + b'a') as char)) }),
        // Ctrl+/ and Ctrl+? in the legacy encoding.
        0x1f => Some(Key { ctrl: true, ..Key::plain(KeyCode::Char('/')) }),
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

/// Decode `11;rgb:RRRR/GGGG/BBBB`, with one to four hex digits a channel.
fn color_reply(body: &str) -> Option<Event> {
    let (slot, value) = body.split_once(';')?;
    let slot: u8 = slot.parse().ok().filter(|slot| matches!(slot, 10 | 11))?;
    let mut channels = value.strip_prefix("rgb:")?.split('/').map(|hex| {
        let max = 16u32.checked_pow(hex.len() as u32)?.checked_sub(1)?;
        let value = u32::from_str_radix(hex, 16).ok()?;
        (!hex.is_empty() && hex.len() <= 4).then(|| (value * 255 + max / 2) / max)
    });
    let mut next = || channels.next().flatten().map(|c| c as u8);
    Some(Event::Color { slot, rgb: (next()?, next()?, next()?) })
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
    if body.is_empty() && matches!(last, b'I' | b'O') {
        return Some(Event::Focus(last == b'I'));
    }
    if body.starts_with('?') {
        // A reply to some other query, such as the keyboard protocol's.
        return None;
    }
    let mut numbers = body.split(';').map(|n| n.parse::<u32>().unwrap_or(0));
    let first = numbers.next().unwrap_or(0);
    let second = numbers.next().unwrap_or(0);
    // Modifiers are sent as 1 + a bit mask.
    let mods = second.saturating_sub(1);
    let (shift, alt, ctrl, cmd) = (mods & 1 != 0, mods & 2 != 0, mods & 4 != 0, mods & 8 != 0);
    let code = match last {
        // The kitty keyboard protocol: a key by its character code. It is
        // how Esc, and keys held with Ctrl, Alt or Command, arrive.
        b'u' => match first {
            27 => KeyCode::Esc,
            13 => KeyCode::Enter,
            9 if shift => KeyCode::BackTab,
            9 => KeyCode::Tab,
            8 | 127 => KeyCode::Backspace,
            code => KeyCode::Char(char::from_u32(code).filter(|c| !c.is_control())?),
        },
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
        // F1 to F4 with modifiers, as CSI 1;5P.
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        b'~' => match first {
            1 | 7 => KeyCode::Home,
            4 | 8 => KeyCode::End,
            3 => KeyCode::Delete,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=15 => KeyCode::F(first as u8 - 10),
            17..=21 => KeyCode::F(first as u8 - 11),
            23 | 24 => KeyCode::F(first as u8 - 12),
            _ => return None,
        },
        _ => return None,
    };
    Some(Event::Key(Key { code, shift, alt, ctrl, cmd }))
}

/// Decode an SGR mouse report, `button;x;y`.
fn mouse(report: &str, pressed: bool) -> Option<Event> {
    let mut numbers = report.split(';').map(|n| n.parse::<u32>().ok());
    let bits = numbers.next()??;
    let x = numbers.next()??;
    let y = numbers.next()??;
    let button = match (bits & 128 != 0, bits & 3) {
        (true, 0) => Some(Button::Back),
        (true, 1) => Some(Button::Forward),
        (true, _) => None,
        (false, 0) => Some(Button::Left),
        (false, 1) => Some(Button::Middle),
        (false, 2) => Some(Button::Right),
        (false, _) => None,
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
    Some(Event::Mouse(Mouse { kind, button, x, y, shift: bits & 4 != 0, alt: bits & 8 != 0, ctrl: bits & 16 != 0 }))
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
        assert_eq!(parse(b"\x1b[1;3D"), std::slice::from_ref(&alt_left));
        assert_eq!(parse(b"\x1bb"), [alt_left]);
        let alt_x = Key { alt: true, ..Key::plain(KeyCode::Char('x')) };
        assert_eq!(parse(b"\x1bx"), [Event::Key(alt_x)]);
    }

    #[test]
    fn function_keys() {
        assert_eq!(parse(b"\x1bOP\x1b[24~\x1b[15~"), [key(KeyCode::F(1)), key(KeyCode::F(12)), key(KeyCode::F(5))]);
        let shift_f12 = Key { shift: true, ..Key::plain(KeyCode::F(12)) };
        assert_eq!(parse(b"\x1b[24;2~"), [Event::Key(shift_f12)]);
        assert_eq!(parse(b"\x1b[1;5P"), [Event::Key(Key { ctrl: true, ..Key::plain(KeyCode::F(1)) })]);
    }

    #[test]
    fn colour_replies() {
        let reply = |bytes: &[u8]| Parser::default().feed(bytes);
        assert_eq!(reply(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\"), [Event::Color { slot: 11, rgb: (30, 30, 46) }]);
        assert_eq!(reply(b"\x1b]10;rgb:ff/80/00\x07"), [Event::Color { slot: 10, rgb: (255, 128, 0) }]);
        // Split across reads, then a key.
        let mut parser = Parser::default();
        assert_eq!(parser.feed(b"\x1b]11;rgb:00"), []);
        assert_eq!(
            parser.feed(b"00/0000/0000\x07x"),
            [Event::Color { slot: 11, rgb: (0, 0, 0) }, key(KeyCode::Char('x'))]
        );
        // Alt+] is still a key.
        assert_eq!(reply(b"\x1b]a").len(), 2);
    }

    #[test]
    fn lone_escape_is_the_esc_key() {
        assert_eq!(parse(b"\x1b"), [key(KeyCode::Esc)]);
    }

    #[test]
    fn mouse_reports() {
        let base = Mouse {
            kind: MouseKind::Press,
            button: Some(Button::Left),
            x: 12,
            y: 3,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert_eq!(parse(b"\x1b[<0;12;3M"), [Event::Mouse(base)]);
        assert_eq!(parse(b"\x1b[<0;12;3m"), [Event::Mouse(Mouse { kind: MouseKind::Release, ..base })]);
        assert_eq!(parse(b"\x1b[<35;12;3M"), [Event::Mouse(Mouse { kind: MouseKind::Move, button: None, ..base })]);
        assert_eq!(parse(b"\x1b[<32;12;3M"), [Event::Mouse(Mouse { kind: MouseKind::Move, ..base })]);
        assert_eq!(parse(b"\x1b[<128;12;3M"), [Event::Mouse(Mouse { button: Some(Button::Back), ..base })]);
        let forward_up = Mouse { kind: MouseKind::Release, button: Some(Button::Forward), ..base };
        assert_eq!(parse(b"\x1b[<129;12;3m"), [Event::Mouse(forward_up)]);
        assert_eq!(
            parse(b"\x1b[<65;12;3M"),
            [Event::Mouse(Mouse { kind: MouseKind::Wheel(0, 1), button: None, ..base })]
        );
        assert_eq!(parse(b"\x1b[<18;12;3M"), [Event::Mouse(Mouse { button: Some(Button::Right), ctrl: true, ..base })]);
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
        assert_eq!(parse(b"\x1b[I\x1b[O"), [Event::Focus(true), Event::Focus(false)]);
        assert_eq!(parse(b"\x1b[?1u"), []);
        let cmd_l = Key { cmd: true, ..Key::plain(KeyCode::Char('l')) };
        let ctrl_l = Key { ctrl: true, ..Key::plain(KeyCode::Char('l')) };
        let shift_enter = Key { shift: true, ..Key::plain(KeyCode::Enter) };
        assert_eq!(
            parse(b"\x1b[108;9u\x1b[108;5u\x1b[27u\x1b[13;2u"),
            [Event::Key(cmd_l), Event::Key(ctrl_l), key(KeyCode::Esc), Event::Key(shift_enter)]
        );
        assert_eq!(parse(b"\x1b_Gi=2;OK\x1b\\a"), [Event::Graphics { id: 2, ok: true }, key(KeyCode::Char('a'))]);
        assert_eq!(parse(b"\x1b_Gi=2;EINVAL: invalid data\x1b\\"), [Event::Graphics { id: 2, ok: false }]);
        assert_eq!(parse(b"\x1b[4;600;800t"), []);
    }
}
