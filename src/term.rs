//! Backend terminal dựa trên `libghostty-vt`, cùng hướng triển khai với Herdr.

use std::sync::{Arc, Mutex};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellWide, Screen};
use libghostty_vt::style::{Palette, RgbColor, StyleColor, Underline};
use libghostty_vt::terminal::{
    ClipboardLocation, ColorScheme, ConformanceLevel, DeviceAttributeFeature, DeviceAttributes,
    DeviceType, Mode,
    PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes, TertiaryDeviceAttributes,
};
use libghostty_vt::{Terminal, TerminalOptions};
use libghostty_vt::{key, mouse};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::terminal_theme::{CellPixelSize, HostTerminalTheme};
use crate::xtgettcap::XtgettcapTracker;

/// Toạ độ một ô trong viewport terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPoint {
    pub row: u16,
    pub col: u16,
}

/// Vùng chọn tuyến tính, tính cả hai đầu mút.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSelection {
    pub anchor: GridPoint,
    pub end: GridPoint,
}

impl GridSelection {
    pub fn new(point: GridPoint) -> Self {
        Self {
            anchor: point,
            end: point,
        }
    }

    fn contains(self, point: GridPoint) -> bool {
        let (start, end) = if self.anchor <= self.end {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        };
        point >= start && point <= end
    }
}

/// Trạng thái emulator terminal cho một pane.
pub struct TermGrid {
    terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    screen: TermScreen,
    host_theme: HostTerminalTheme,
    cell_size: CellPixelSize,
    size_report: Arc<Mutex<libghostty_vt::terminal::SizeReportSize>>,
    pending_effects: Arc<Mutex<TerminalEffects>>,
    xtgettcap: XtgettcapTracker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TermColor {
    Indexed(u8),
    Rgb(RgbColor),
}

impl TermColor {
    fn ratatui(self) -> Color {
        match self {
            Self::Indexed(index) => Color::Indexed(index),
            Self::Rgb(color) => Color::Rgb(color.r, color.g, color.b),
        }
    }
}

#[derive(Clone, Default)]
struct TermCell {
    contents: String,
    fg: Option<TermColor>,
    bg: Option<TermColor>,
    bold: bool,
    faint: bool,
    italic: bool,
    underline: bool,
    underline_color: Option<TermColor>,
    blink: bool,
    invisible: bool,
    strikethrough: bool,
    inverse: bool,
    wide: CellWideKind,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CellWideKind {
    #[default]
    Narrow,
    Wide,
    Spacer,
}

/// Snapshot nhẹ của viewport Ghostty để UI chỉ cần mượn bất biến khi render.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

pub struct TermScreen {
    rows: u16,
    cols: u16,
    cells: Vec<TermCell>,
    cursor: (u16, u16),
    hide_cursor: bool,
    cursor_shape: CursorShape,
    cursor_blinking: bool,
    cursor_color: Option<RgbColor>,
    default_fg: Color,
    default_bg: Color,
    #[cfg_attr(not(test), allow(dead_code))]
    contents: String,
}

/// Hiệu ứng terminal cần adapter ngoài thực thi sau khi Ghostty xử lý PTY output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardTarget {
    Standard,
    Primary,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ClipboardWriteRequest {
    pub target: ClipboardTarget,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct TerminalEffects {
    pub pty_responses: Vec<Vec<u8>>,
    pub bell_count: usize,
    pub clipboard_writes: Vec<ClipboardWriteRequest>,
    pub title: Option<String>,
    pub cwd: Option<String>,
}

impl TerminalEffects {
    fn append(&mut self, mut other: Self) {
        self.pty_responses.append(&mut other.pty_responses);
        self.bell_count += other.bell_count;
        self.clipboard_writes.append(&mut other.clipboard_writes);
        if other.title.is_some() {
            self.title = other.title;
        }
        if other.cwd.is_some() {
            self.cwd = other.cwd;
        }
    }
}

impl TermGrid {
    #[cfg(test)]
    pub fn new(rows: u16, cols: u16, scrollback_limit_bytes: usize) -> Self {
        Self::with_host_theme(
            rows,
            cols,
            scrollback_limit_bytes,
            HostTerminalTheme::default(),
        )
    }

    #[cfg(test)]
    pub fn with_host_theme(
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_theme: HostTerminalTheme,
    ) -> Self {
        Self::with_host_capabilities(
            rows,
            cols,
            scrollback_limit_bytes,
            host_theme,
            CellPixelSize::default(),
        )
    }

    pub fn with_host_capabilities(
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_theme: HostTerminalTheme,
        cell_size: CellPixelSize,
    ) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let pending_effects = Arc::new(Mutex::new(TerminalEffects::default()));
        let size_report = Arc::new(Mutex::new(libghostty_vt::terminal::SizeReportSize {
            rows,
            columns: cols,
            cell_width: cell_size.width,
            cell_height: cell_size.height,
        }));
        let callback_effects = Arc::clone(&pending_effects);
        let mut terminal = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: scrollback_limit_bytes,
        })
        .expect("không thể tạo libghostty-vt terminal");
        terminal
            .on_pty_write(move |_terminal, bytes| {
                if let Ok(mut effects) = callback_effects.lock() {
                    effects.pty_responses.push(bytes.to_vec());
                }
            })
            .expect("không thể đăng ký callback phản hồi PTY");
        let callback_effects = Arc::clone(&pending_effects);
        terminal
            .on_bell(move |_terminal| {
                if let Ok(mut effects) = callback_effects.lock() {
                    effects.bell_count += 1;
                }
            })
            .expect("không thể đăng ký callback bell");
        let callback_effects = Arc::clone(&pending_effects);
        terminal
            .on_title_changed(move |terminal| {
                if let Ok(mut effects) = callback_effects.lock() {
                    effects.title = terminal.title().ok().map(str::to_string);
                }
            })
            .expect("không thể đăng ký callback title");
        let callback_effects = Arc::clone(&pending_effects);
        terminal
            .on_pwd_changed(move |terminal| {
                if let Ok(mut effects) = callback_effects.lock() {
                    effects.cwd = terminal.pwd().ok().map(str::to_string);
                }
            })
            .expect("không thể đăng ký callback cwd");
        let callback_effects = Arc::clone(&pending_effects);
        terminal
            .on_clipboard_write(move |_terminal, write| {
                if let Some(content) = write.contents().find(|content| {
                    content.mime.is_empty() || content.mime == "text/plain"
                }) && let Ok(mut effects) = callback_effects.lock()
                {
                    let target = match write.location() {
                        ClipboardLocation::Standard => ClipboardTarget::Standard,
                        ClipboardLocation::Selection | ClipboardLocation::Primary => {
                            ClipboardTarget::Primary
                        }
                    };
                    effects.clipboard_writes.push(ClipboardWriteRequest {
                        target,
                        data: content.data.as_bytes().to_vec(),
                    });
                }
                Ok(())
            })
            .expect("không thể đăng ký callback clipboard");
        terminal
            .on_xtversion(|_terminal| Some(concat!("termul ", env!("CARGO_PKG_VERSION"))))
            .expect("không thể đăng ký callback XTVERSION");
        let callback_size = Arc::clone(&size_report);
        terminal
            .on_size(move |_terminal| callback_size.lock().ok().map(|size| *size))
            .expect("không thể đăng ký callback size report");
        let color_scheme = host_theme.background.map(|color| {
            let luminance =
                u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114;
            if luminance >= 128_000 {
                ColorScheme::Light
            } else {
                ColorScheme::Dark
            }
        });
        terminal
            .on_color_scheme(move |_terminal| color_scheme)
            .expect("không thể đăng ký callback color scheme");
        terminal
            .on_device_attributes(|_terminal| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT100,
                        &[DeviceAttributeFeature(2)],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 0,
                        rom_cartridge: 0,
                    },
                    tertiary: TertiaryDeviceAttributes::default(),
                })
            })
            .expect("không thể đăng ký callback device attributes");
        let mut grid = Self {
            terminal,
            render_state: RenderState::new().expect("không thể tạo Ghostty render state"),
            row_iterator: RowIterator::new().expect("không thể tạo Ghostty row iterator"),
            cell_iterator: CellIterator::new().expect("không thể tạo Ghostty cell iterator"),
            key_encoder: key::Encoder::new().expect("không thể tạo Ghostty key encoder"),
            key_event: key::Event::new().expect("không thể tạo Ghostty key event"),
            mouse_encoder: mouse::Encoder::new().expect("không thể tạo Ghostty mouse encoder"),
            mouse_event: mouse::Event::new().expect("không thể tạo Ghostty mouse event"),
            screen: TermScreen::empty(rows, cols),
            host_theme,
            cell_size,
            size_report,
            pending_effects,
            xtgettcap: XtgettcapTracker::default(),
        };
        grid.apply_host_theme(host_theme);
        grid
    }

    pub fn apply_host_theme(&mut self, theme: HostTerminalTheme) {
        self.host_theme = theme;
        let mut palette = Palette::default();
        for (index, color) in theme.palette.into_iter().enumerate() {
            if let Some(color) = color {
                palette.0[index] = color;
            }
        }
        self.terminal
            .set_default_fg_color(theme.foreground)
            .and_then(|terminal| terminal.set_default_bg_color(theme.background))
            .and_then(|terminal| terminal.set_default_cursor_color(theme.cursor))
            .and_then(|terminal| terminal.set_default_color_palette(Some(palette)))
            .expect("không thể áp bảng màu terminal host");
        self.refresh();
    }

    pub fn process(&mut self, bytes: &[u8]) -> TerminalEffects {
        self.xtgettcap.observe(bytes);
        let xtgettcap = self.xtgettcap.drain();
        let mut effects = TerminalEffects::default();
        let mut written = 0;
        for response in xtgettcap {
            let end = response.end_offset.min(bytes.len());
            if end > written {
                self.terminal.vt_write(&bytes[written..end]);
                effects.append(self.drain_effects());
                written = end;
            }
            effects.pty_responses.push(response.bytes);
        }
        if written < bytes.len() {
            self.terminal.vt_write(&bytes[written..]);
            effects.append(self.drain_effects());
        }
        self.refresh();
        effects
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> TerminalEffects {
        if rows == 0 || cols == 0 || self.screen.size() == (rows, cols) {
            return TerminalEffects::default();
        }
        let offset_from_bottom = self.scrollback();
        if let Ok(mut size) = self.size_report.lock() {
            size.rows = rows;
            size.columns = cols;
        }
        self.terminal
            .resize(cols, rows, self.cell_size.width, self.cell_size.height)
            .expect("libghostty-vt resize thất bại");
        if offset_from_bottom == 0 {
            self.refresh();
        } else {
            self.set_scrollback(offset_from_bottom);
        }
        self.drain_effects()
    }

    fn drain_effects(&self) -> TerminalEffects {
        self.pending_effects
            .lock()
            .map(|mut effects| std::mem::take(&mut *effects))
            .unwrap_or_default()
    }

    pub fn screen(&self) -> &TermScreen {
        &self.screen
    }

    /// Số dòng viewport đang cách đáy scrollback.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scrollback(&self) -> usize {
        self.terminal
            .scrollbar()
            .map(|bar| (bar.total - bar.len - bar.offset) as usize)
            .unwrap_or(0)
    }

    /// Đặt vị trí viewport theo số dòng tính từ đáy.
    pub fn set_scrollback(&mut self, rows: usize) {
        let Ok(bar) = self.terminal.scrollbar() else {
            return;
        };
        let max_offset = (bar.total - bar.len) as usize;
        self.terminal
            .scroll_viewport(ScrollViewport::Row(max_offset.saturating_sub(rows)));
        self.refresh();
    }

    /// Cuộn viewport; delta âm là lên lịch sử, dương là về nội dung mới.
    pub fn scroll_lines(&mut self, delta: isize) {
        self.terminal.scroll_viewport(ScrollViewport::Delta(delta));
        self.refresh();
    }

    pub fn in_alt_screen(&self) -> bool {
        self.terminal
            .active_screen()
            .is_ok_and(|screen| screen == Screen::Alternate)
    }

    pub fn has_mouse_tracking(&self) -> bool {
        self.terminal.is_mouse_tracking().unwrap_or(false)
    }

    pub fn focus_report(&self, focused: bool) -> Option<&'static [u8]> {
        self.terminal
            .mode(Mode::FOCUS_EVENT)
            .unwrap_or(false)
            .then_some(if focused { b"\x1b[I" } else { b"\x1b[O" })
    }

    pub fn synchronized_output(&self) -> bool {
        self.terminal
            .mode(Mode::SYNC_OUTPUT)
            .unwrap_or(false)
    }

    /// Ứng dụng trong pane có yêu cầu bọc nội dung paste bằng DEC mode 2004 hay không.
    pub fn has_bracketed_paste(&self) -> bool {
        self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false)
    }

    /// Mã hoá phím theo keyboard protocol hiện hành của ứng dụng trong pane.
    /// Ghostty tự áp dụng legacy, modifyOtherKeys hoặc Kitty keyboard protocol.
    pub fn encode_key(&mut self, event: KeyEvent) -> Option<Vec<u8>> {
        let (key, text, unshifted) = convert_key_code(event.code)?;
        let action = match event.kind {
            KeyEventKind::Press => key::Action::Press,
            KeyEventKind::Repeat => key::Action::Repeat,
            KeyEventKind::Release => key::Action::Release,
        };
        let mut mods = convert_key_modifiers(event.modifiers);
        if event.state.contains(KeyEventState::CAPS_LOCK) {
            mods.insert(key::Mods::CAPS_LOCK);
        }
        if event.state.contains(KeyEventState::NUM_LOCK) {
            mods.insert(key::Mods::NUM_LOCK);
        }
        if matches!(event.code, KeyCode::BackTab) {
            mods.insert(key::Mods::SHIFT);
        }

        self.key_event
            .set_action(action)
            .set_key(key)
            .set_mods(mods)
            .set_consumed_mods(key::Mods::empty())
            .set_utf8(text);
        if let Some(codepoint) = unshifted {
            self.key_event.set_unshifted_codepoint(codepoint);
        }

        let mut bytes = Vec::with_capacity(16);
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&self.key_event, &mut bytes)
            .ok()?;
        (!bytes.is_empty()).then_some(bytes)
    }

    /// Mã hoá mouse event theo đúng tracking mode/format mà app đã bật.
    /// Ghostty tự chọn X10, UTF-8, SGR, urxvt hoặc SGR-pixels từ terminal state.
    pub fn encode_mouse(&mut self, event: MouseEvent, inner: Rect) -> Option<Vec<u8>> {
        if event.column < inner.x
            || event.row < inner.y
            || event.column >= inner.x + inner.width
            || event.row >= inner.y + inner.height
        {
            return None;
        }

        let (action, button) = match event.kind {
            MouseEventKind::Down(button) => (mouse::Action::Press, convert_mouse_button(button)),
            MouseEventKind::Up(button) => (mouse::Action::Release, convert_mouse_button(button)),
            MouseEventKind::Drag(button) => (mouse::Action::Motion, convert_mouse_button(button)),
            MouseEventKind::Moved => (mouse::Action::Motion, None),
            MouseEventKind::ScrollUp => (mouse::Action::Press, Some(mouse::Button::Four)),
            MouseEventKind::ScrollDown => (mouse::Action::Press, Some(mouse::Button::Five)),
            MouseEventKind::ScrollLeft => (mouse::Action::Press, Some(mouse::Button::Six)),
            MouseEventKind::ScrollRight => (mouse::Action::Press, Some(mouse::Button::Seven)),
        };
        let mut mods = key::Mods::empty();
        if event.modifiers.contains(KeyModifiers::SHIFT) {
            mods.insert(key::Mods::SHIFT);
        }
        if event.modifiers.contains(KeyModifiers::ALT) {
            mods.insert(key::Mods::ALT);
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            mods.insert(key::Mods::CTRL);
        }

        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mods)
            .set_position(mouse::Position {
                x: f32::from(event.column - inner.x),
                y: f32::from(event.row - inner.y),
            });
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal);
        // Crossterm chỉ cung cấp vị trí theo ô, trong khi Ghostty nhận vị trí
        // surface-space. Dùng hình học 1×1 để không chia tọa độ thêm lần nữa.
        // Nếu child yêu cầu SGR-pixels, hạ xuống SGR vì không có pixel chính xác.
        if self
            .terminal
            .mode(Mode::SGR_PIXELS_MOUSE)
            .unwrap_or(false)
        {
            self.mouse_encoder.set_format(mouse::Format::Sgr);
        }
        self.mouse_encoder
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(inner.width),
                screen_height: u32::from(inner.height),
                cell_width: 1,
                cell_height: 1,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .set_any_button_pressed(matches!(event.kind, MouseEventKind::Drag(_)));
        let mut bytes = Vec::with_capacity(32);
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut bytes)
            .ok()?;
        (!bytes.is_empty()).then_some(bytes)
    }

    fn refresh(&mut self) {
        let mut cells = Vec::new();
        let (
            rows,
            cols,
            cursor,
            hide_cursor,
            cursor_shape,
            cursor_blinking,
            cursor_color,
            default_fg,
            default_bg,
            contents,
        ) = {
            let snapshot = self
                .render_state
                .update(&self.terminal)
                .expect("không thể cập nhật Ghostty render state");
            let rows = snapshot.rows().unwrap_or(0);
            let cols = snapshot.cols().unwrap_or(0);
            let active_palette = self
                .terminal
                .color_palette()
                .expect("không thể đọc bảng màu hiện hành Ghostty");
            let default_palette = self
                .terminal
                .default_color_palette()
                .expect("không thể đọc bảng màu mặc định Ghostty");
            let mut effective_fg = self.terminal.fg_color().ok().flatten();
            let mut effective_bg = self.terminal.bg_color().ok().flatten();
            if self.terminal.mode(Mode::REVERSE_COLORS).unwrap_or(false) {
                std::mem::swap(&mut effective_fg, &mut effective_bg);
            }
            let default_fg = resolved_default_color(effective_fg, self.host_theme.foreground);
            let default_bg = resolved_default_color(effective_bg, self.host_theme.background);
            cells.reserve(rows as usize * cols as usize);

            let mut row_iter = self
                .row_iterator
                .update(&snapshot)
                .expect("không thể đọc hàng Ghostty");
            let mut contents = String::new();
            let mut row_index = 0_u16;
            while let Some(row) = row_iter.next() {
                let mut cell_iter = self
                    .cell_iterator
                    .update(row)
                    .expect("không thể đọc cell Ghostty");
                while let Some(cell) = cell_iter.next() {
                    let style = cell.style().unwrap_or_default();
                    let text: String = cell.graphemes().unwrap_or_default().into_iter().collect();
                    if !matches!(
                        cell.raw_cell().and_then(|c| c.wide()),
                        Ok(CellWide::SpacerTail)
                    ) {
                        contents.push_str(&text);
                    }
                    let raw_cell = cell.raw_cell().ok();
                    let wide = match raw_cell.and_then(|cell| cell.wide().ok()) {
                        Some(CellWide::Wide) => CellWideKind::Wide,
                        Some(CellWide::SpacerTail | CellWide::SpacerHead) => CellWideKind::Spacer,
                        _ => CellWideKind::Narrow,
                    };
                    let fg =
                        resolve_style_color(style.fg_color, &active_palette.0, &default_palette.0);
                    let bg = raw_cell
                        .and_then(|cell| match cell.content_tag().ok()? {
                            CellContentTag::BgColorPalette => {
                                cell.bg_color_palette().ok().and_then(|index| {
                                    resolve_palette_color(
                                        index.0,
                                        &active_palette.0,
                                        &default_palette.0,
                                    )
                                })
                            }
                            CellContentTag::BgColorRgb => {
                                cell.bg_color_rgb().ok().map(TermColor::Rgb)
                            }
                            CellContentTag::Codepoint | CellContentTag::CodepointGrapheme => None,
                        })
                        .or_else(|| {
                            resolve_style_color(
                                style.bg_color,
                                &active_palette.0,
                                &default_palette.0,
                            )
                        });
                    let underline_color = resolve_style_color(
                        style.underline_color,
                        &active_palette.0,
                        &default_palette.0,
                    );
                    cells.push(TermCell {
                        contents: text,
                        fg,
                        bg,
                        bold: style.bold,
                        faint: style.faint,
                        italic: style.italic,
                        underline: style.underline != Underline::None,
                        underline_color,
                        blink: style.blink,
                        invisible: style.invisible,
                        strikethrough: style.strikethrough,
                        inverse: style.inverse,
                        wide,
                    });
                }
                cells.resize_with((row_index as usize + 1) * cols as usize, TermCell::default);
                if row_index + 1 < rows {
                    contents.push('\n');
                }
                row_index += 1;
            }
            cells.resize_with(rows as usize * cols as usize, TermCell::default);
            let cursor = snapshot
                .cursor_viewport()
                .ok()
                .flatten()
                .map(|cursor| (cursor.y, cursor.x))
                .unwrap_or((0, 0));
            let hide_cursor = !snapshot.cursor_visible().unwrap_or(false)
                || snapshot.cursor_viewport().ok().flatten().is_none();
            let cursor_shape = match snapshot.cursor_visual_style().ok() {
                Some(CursorVisualStyle::Bar) => CursorShape::Bar,
                Some(CursorVisualStyle::Underline) => CursorShape::Underline,
                Some(CursorVisualStyle::Block | CursorVisualStyle::BlockHollow) | Some(_) | None => {
                    CursorShape::Block
                }
            };
            let cursor_blinking = snapshot.cursor_blinking().unwrap_or(false);
            let cursor_color = self.terminal.cursor_color().ok().flatten();
            (
                rows,
                cols,
                cursor,
                hide_cursor,
                cursor_shape,
                cursor_blinking,
                cursor_color,
                default_fg,
                default_bg,
                contents,
            )
        };

        self.screen = TermScreen {
            rows,
            cols,
            cells,
            cursor,
            hide_cursor,
            cursor_shape,
            cursor_blinking,
            cursor_color,
            default_fg,
            default_bg,
            contents,
        };
    }
}

fn resolved_default_color(color: Option<RgbColor>, host_color: Option<RgbColor>) -> Color {
    match (color, host_color) {
        (Some(color), Some(host)) if color != host => Color::Rgb(color.r, color.g, color.b),
        _ => Color::Reset,
    }
}

fn resolve_style_color(
    color: StyleColor,
    active_palette: &[RgbColor; 256],
    default_palette: &[RgbColor; 256],
) -> Option<TermColor> {
    match color {
        StyleColor::None => None,
        StyleColor::Palette(index) => {
            resolve_palette_color(index.0, active_palette, default_palette)
        }
        StyleColor::Rgb(color) => Some(TermColor::Rgb(color)),
    }
}

fn resolve_palette_color(
    index: u8,
    active_palette: &[RgbColor; 256],
    default_palette: &[RgbColor; 256],
) -> Option<TermColor> {
    let index_usize = usize::from(index);
    Some(
        if active_palette[index_usize] == default_palette[index_usize] {
            TermColor::Indexed(index)
        } else {
            TermColor::Rgb(active_palette[index_usize])
        },
    )
}

fn convert_key_modifiers(modifiers: KeyModifiers) -> key::Mods {
    let mut mods = key::Mods::empty();
    if modifiers.contains(KeyModifiers::SHIFT) {
        mods.insert(key::Mods::SHIFT);
    }
    if modifiers.contains(KeyModifiers::ALT) {
        mods.insert(key::Mods::ALT);
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        mods.insert(key::Mods::CTRL);
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        mods.insert(key::Mods::SUPER);
    }
    mods
}

fn convert_key_code(code: KeyCode) -> Option<(key::Key, Option<String>, Option<char>)> {
    let plain = |key| Some((key, None, None));
    match code {
        KeyCode::Backspace => plain(key::Key::Backspace),
        KeyCode::Enter => plain(key::Key::Enter),
        KeyCode::Left => plain(key::Key::ArrowLeft),
        KeyCode::Right => plain(key::Key::ArrowRight),
        KeyCode::Up => plain(key::Key::ArrowUp),
        KeyCode::Down => plain(key::Key::ArrowDown),
        KeyCode::Home => plain(key::Key::Home),
        KeyCode::End => plain(key::Key::End),
        KeyCode::PageUp => plain(key::Key::PageUp),
        KeyCode::PageDown => plain(key::Key::PageDown),
        KeyCode::Tab | KeyCode::BackTab => plain(key::Key::Tab),
        KeyCode::Delete => plain(key::Key::Delete),
        KeyCode::Insert => plain(key::Key::Insert),
        KeyCode::Esc => plain(key::Key::Escape),
        KeyCode::F(n @ 1..=12) => plain([
            key::Key::F1,
            key::Key::F2,
            key::Key::F3,
            key::Key::F4,
            key::Key::F5,
            key::Key::F6,
            key::Key::F7,
            key::Key::F8,
            key::Key::F9,
            key::Key::F10,
            key::Key::F11,
            key::Key::F12,
        ][usize::from(n - 1)]),
        KeyCode::Char(c) => {
            let (key, unshifted) = convert_char_key(c);
            Some((key, Some(c.to_string()), Some(unshifted)))
        }
        _ => None,
    }
}

fn convert_char_key(c: char) -> (key::Key, char) {
    let lower = c.to_ascii_lowercase();
    let letter = match lower {
        'a'..='z' => Some([
            key::Key::A,
            key::Key::B,
            key::Key::C,
            key::Key::D,
            key::Key::E,
            key::Key::F,
            key::Key::G,
            key::Key::H,
            key::Key::I,
            key::Key::J,
            key::Key::K,
            key::Key::L,
            key::Key::M,
            key::Key::N,
            key::Key::O,
            key::Key::P,
            key::Key::Q,
            key::Key::R,
            key::Key::S,
            key::Key::T,
            key::Key::U,
            key::Key::V,
            key::Key::W,
            key::Key::X,
            key::Key::Y,
            key::Key::Z,
        ][lower as usize - 'a' as usize]),
        _ => None,
    };
    if let Some(key) = letter {
        return (key, lower);
    }

    match c {
        '0' | ')' => (key::Key::Digit0, '0'),
        '1' | '!' => (key::Key::Digit1, '1'),
        '2' | '@' => (key::Key::Digit2, '2'),
        '3' | '#' => (key::Key::Digit3, '3'),
        '4' | '$' => (key::Key::Digit4, '4'),
        '5' | '%' => (key::Key::Digit5, '5'),
        '6' | '^' => (key::Key::Digit6, '6'),
        '7' | '&' => (key::Key::Digit7, '7'),
        '8' | '*' => (key::Key::Digit8, '8'),
        '9' | '(' => (key::Key::Digit9, '9'),
        ' ' => (key::Key::Space, ' '),
        '`' | '~' => (key::Key::Backquote, '`'),
        '\\' | '|' => (key::Key::Backslash, '\\'),
        '[' | '{' => (key::Key::BracketLeft, '['),
        ']' | '}' => (key::Key::BracketRight, ']'),
        ',' | '<' => (key::Key::Comma, ','),
        '=' | '+' => (key::Key::Equal, '='),
        '-' | '_' => (key::Key::Minus, '-'),
        '.' | '>' => (key::Key::Period, '.'),
        '\'' | '"' => (key::Key::Quote, '\''),
        ';' | ':' => (key::Key::Semicolon, ';'),
        '/' | '?' => (key::Key::Slash, '/'),
        _ => (key::Key::Unidentified, c),
    }
}

fn convert_mouse_button(button: MouseButton) -> Option<mouse::Button> {
    Some(match button {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
    })
}

impl TermScreen {
    fn empty(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            cells: vec![TermCell::default(); rows as usize * cols as usize],
            cursor: (0, 0),
            hide_cursor: false,
            cursor_shape: CursorShape::Block,
            cursor_blinking: false,
            cursor_color: None,
            default_fg: Color::Reset,
            default_bg: Color::Reset,
            contents: String::new(),
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.cursor
    }

    pub fn hide_cursor(&self) -> bool {
        self.hide_cursor
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    pub fn cursor_blinking(&self) -> bool {
        self.cursor_blinking
    }

    pub fn cursor_color(&self) -> Option<RgbColor> {
        self.cursor_color
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn contents(&self) -> &str {
        &self.contents
    }

    fn cell(&self, row: u16, col: u16) -> Option<&TermCell> {
        (row < self.rows && col < self.cols)
            .then(|| &self.cells[row as usize * self.cols as usize + col as usize])
    }

    /// Text của một ô trong viewport (rỗng nếu ô trống hoặc là spacer sau ký tự rộng).
    pub fn cell_text(&self, row: u16, col: u16) -> Option<&str> {
        self.cell(row, col).map(|cell| cell.contents.as_str())
    }

    /// Lấy nội dung text của vùng chọn trong viewport hiện tại.
    pub fn selected_text(&self, selection: GridSelection) -> String {
        let (start, end) = if selection.anchor <= selection.end {
            (selection.anchor, selection.end)
        } else {
            (selection.end, selection.anchor)
        };
        let mut lines = Vec::new();
        for row in start.row..=end.row.min(self.rows.saturating_sub(1)) {
            let first_col = if row == start.row { start.col } else { 0 };
            let last_col = if row == end.row {
                end.col
            } else {
                self.cols.saturating_sub(1)
            };
            let mut line = String::new();
            for col in first_col..=last_col.min(self.cols.saturating_sub(1)) {
                let Some(cell) = self.cell(row, col) else {
                    continue;
                };
                if cell.wide == CellWideKind::Spacer {
                    continue;
                }
                if cell.contents.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&cell.contents);
                }
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }
}

/// Widget vẽ snapshot viewport của Ghostty vào một `Rect`.
pub struct TermView<'a> {
    pub screen: &'a TermScreen,
    pub selection: Option<GridSelection>,
    pub default_bg: Color,
}

fn blend_selection_bg(bg: Color, default_bg: Color) -> Color {
    let Color::Rgb(r, g, b) = bg else {
        return blend_selection_bg(default_bg, Color::Rgb(40, 42, 54));
    };
    let blend = |channel: u8| ((u16::from(channel) * 7 + 128 * 3) / 10) as u8;
    Color::Rgb(blend(r), blend(g), blend(b))
}

impl Widget for TermView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for row in 0..rows.min(area.height) {
            for col in 0..cols.min(area.width) {
                let Some(cell) = self.screen.cell(row, col) else {
                    continue;
                };
                if cell.wide == CellWideKind::Spacer {
                    continue;
                }
                let x = area.x + col;
                let y = area.y + row;
                if let Some(buf_cell) = buf.cell_mut((x, y)) {
                    buf_cell.reset();
                    buf_cell.set_symbol(if cell.contents.is_empty() {
                        " "
                    } else {
                        &cell.contents
                    });
                    let mut fg = cell.fg.map_or(self.screen.default_fg, TermColor::ratatui);
                    let mut bg = cell.bg.map_or(self.screen.default_bg, TermColor::ratatui);
                    let selected = self
                        .selection
                        .is_some_and(|selection| selection.contains(GridPoint { row, col }));
                    let reverse_defaults = cell.inverse && fg == bg && !selected;
                    if cell.inverse && !reverse_defaults {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    if selected {
                        bg = blend_selection_bg(bg, self.default_bg);
                    }
                    let mut style = Style::default().fg(fg).bg(bg);
                    if let Some(color) = cell.underline_color {
                        style = style.underline_color(color.ratatui());
                    }
                    if reverse_defaults {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    if cell.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if cell.faint {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    if cell.italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    if cell.underline {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    if cell.blink {
                        style = style.add_modifier(Modifier::SLOW_BLINK);
                    }
                    if cell.invisible {
                        style = style.add_modifier(Modifier::HIDDEN);
                    }
                    if cell.strikethrough {
                        style = style.add_modifier(Modifier::CROSSED_OUT);
                    }
                    buf_cell.set_style(style);
                }
                if cell.wide == CellWideKind::Wide
                    && x + 1 < area.x + area.width
                    && let Some(next) = buf.cell_mut((x + 1, y))
                {
                    next.set_symbol("");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;

    #[test]
    fn renders_truecolor_and_dim_attributes() {
        let area = Rect::new(0, 0, 8, 1);
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[2;38;2;12;34;56mX");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: None,
            default_bg: Color::Rgb(40, 42, 54),
        }
        .render(area, &mut buf);
        let cell = &buf[(0, 0)];
        assert_eq!(cell.fg, Color::Rgb(12, 34, 56));
        assert!(cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn renders_extended_text_attributes_and_underline_color() {
        let area = Rect::new(0, 0, 1, 1);
        let mut grid = TermGrid::new(1, 1, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[3;4;5;8;9;58;2;10;20;30mX");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: None,
            default_bg: Color::Reset,
        }
        .render(area, &mut buf);

        let cell = &buf[(0, 0)];
        assert_eq!(cell.underline_color, Color::Rgb(10, 20, 30));
        assert!(cell.modifier.contains(Modifier::ITALIC));
        assert!(cell.modifier.contains(Modifier::UNDERLINED));
        assert!(cell.modifier.contains(Modifier::SLOW_BLINK));
        assert!(cell.modifier.contains(Modifier::HIDDEN));
        assert!(cell.modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn selection_contains_points_in_both_drag_directions() {
        let start = GridPoint { row: 1, col: 2 };
        let end = GridPoint { row: 2, col: 1 };
        let forward = GridSelection { anchor: start, end };
        let backward = GridSelection {
            anchor: end,
            end: start,
        };

        for selection in [forward, backward] {
            assert!(selection.contains(start));
            assert!(selection.contains(GridPoint { row: 1, col: 7 }));
            assert!(selection.contains(end));
            assert!(!selection.contains(GridPoint { row: 1, col: 1 }));
            assert!(!selection.contains(GridPoint { row: 2, col: 2 }));
        }
    }

    #[test]
    fn selection_blends_gray_over_rendered_background() {
        let area = Rect::new(0, 0, 2, 1);
        let mut grid = TermGrid::new(1, 2, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[38;2;12;34;56;48;2;60;70;80mXY");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: Some(GridSelection {
                anchor: GridPoint { row: 0, col: 0 },
                end: GridPoint { row: 0, col: 0 },
            }),
            default_bg: Color::Rgb(40, 42, 54),
        }
        .render(area, &mut buf);

        assert_eq!(buf[(0, 0)].fg, Color::Rgb(12, 34, 56));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(80, 87, 94));
        assert_eq!(buf[(1, 0)].fg, Color::Rgb(12, 34, 56));
        assert_eq!(buf[(1, 0)].bg, Color::Rgb(60, 70, 80));
    }

    #[test]
    fn selection_blends_gray_over_default_terminal_background() {
        let area = Rect::new(0, 0, 1, 1);
        let mut grid = TermGrid::new(1, 1, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"X");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: Some(GridSelection::new(GridPoint { row: 0, col: 0 })),
            default_bg: Color::Rgb(40, 42, 54),
        }
        .render(area, &mut buf);

        assert_eq!(buf[(0, 0)].bg, Color::Rgb(66, 67, 76));
    }

    #[test]
    fn selected_text_handles_reverse_drag_rows_and_wide_cells() {
        let mut grid = TermGrid::new(2, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process("ab中 d\r\nef".as_bytes());
        let selection = GridSelection {
            anchor: GridPoint { row: 1, col: 1 },
            end: GridPoint { row: 0, col: 1 },
        };

        assert_eq!(grid.screen().selected_text(selection), "b中 d\nef");
    }

    #[test]
    fn encodes_shift_enter_with_live_keyboard_protocol() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);

        let effects = grid.process(b"\x1b[>7u\x1b[?u");
        assert_eq!(effects.pty_responses, [b"\x1b[?7u".to_vec()]);
        assert_eq!(grid.encode_key(shift_enter), Some(b"\x1b[13;2u".to_vec()));
    }

    #[test]
    fn kitty_protocol_encodes_modifiers_and_key_releases() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[>7u");

        assert_eq!(
            grid.encode_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL)),
            Some(b"\x1b[9;5u".to_vec())
        );
        assert!(
            grid.encode_key(KeyEvent::new_with_kind(
                KeyCode::Up,
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ))
            .is_some()
        );
    }

    #[test]
    fn encodes_shift_enter_with_modify_other_keys_fallback() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[>4;2m");

        assert_eq!(
            grid.encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(b"\x1b[27;2;13~".to_vec())
        );
    }

    #[test]
    fn surfaces_terminal_effects_from_ghostty_callbacks() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        let effects = grid.process(
            b"\x07\x1b]0;build log\x07\x1b]7;file://localhost/tmp\x1b\\\x1b]52;c;aGVsbG8=\x1b\\",
        );

        assert_eq!(effects.bell_count, 1);
        assert_eq!(effects.title.as_deref(), Some("build log"));
        assert_eq!(effects.cwd.as_deref(), Some("file://localhost/tmp"));
        assert_eq!(
            effects.clipboard_writes,
            [ClipboardWriteRequest {
                target: ClipboardTarget::Standard,
                data: b"hello".to_vec(),
            }]
        );
    }

    #[test]
    fn keeps_host_palette_index_and_flattens_child_override() {
        let mut theme = HostTerminalTheme::default();
        theme.palette[1] = Some(RgbColor {
            r: 12,
            g: 34,
            b: 56,
        });
        let mut grid = TermGrid::with_host_theme(1, 2, TEST_SCROLLBACK_LIMIT_BYTES, theme);
        grid.process(b"\x1b[31mX");

        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: None,
            default_bg: Color::Reset,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(1));

        grid.process(b"\x1b]4;1;rgb:aaaa/bbbb/cccc\x1b\\\r\x1b[31mY");
        let mut buf = Buffer::empty(area);
        TermView {
            screen: grid.screen(),
            selection: None,
            default_bg: Color::Reset,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(0xaa, 0xbb, 0xcc));
    }

    #[test]
    fn reverse_screen_mode_swaps_host_default_colors() {
        let theme = HostTerminalTheme {
            foreground: Some(RgbColor { r: 220, g: 221, b: 222 }),
            background: Some(RgbColor { r: 10, g: 11, b: 12 }),
            ..HostTerminalTheme::default()
        };
        let mut grid = TermGrid::with_host_theme(1, 1, TEST_SCROLLBACK_LIMIT_BYTES, theme);
        grid.process(b"\x1b[?5h");
        assert_eq!(grid.screen.default_fg, Color::Rgb(10, 11, 12));
        assert_eq!(grid.screen.default_bg, Color::Rgb(220, 221, 222));
    }

    #[test]
    fn host_default_uses_reset_but_child_override_uses_rgb() {
        let theme = HostTerminalTheme {
            background: Some(RgbColor {
                r: 40,
                g: 42,
                b: 54,
            }),
            ..HostTerminalTheme::default()
        };
        let mut grid = TermGrid::with_host_theme(1, 1, TEST_SCROLLBACK_LIMIT_BYTES, theme);
        assert_eq!(grid.screen.default_bg, Color::Reset);

        grid.process(b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\");
        assert_eq!(grid.screen.default_bg, Color::Rgb(0xaa, 0xbb, 0xcc));
    }

    #[test]
    fn tracks_focus_and_synchronized_output_modes() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        assert_eq!(grid.focus_report(true), None);
        assert!(!grid.synchronized_output());

        grid.process(b"\x1b[?1004h\x1b[?2026h");
        assert_eq!(grid.focus_report(true), Some(b"\x1b[I".as_slice()));
        assert_eq!(grid.focus_report(false), Some(b"\x1b[O".as_slice()));
        assert!(grid.synchronized_output());

        grid.process(b"\x1b[?1004l\x1b[?2026l");
        assert_eq!(grid.focus_report(true), None);
        assert!(!grid.synchronized_output());
    }

    #[test]
    fn reports_cell_and_grid_size_from_host_capabilities() {
        let mut grid = TermGrid::with_host_capabilities(
            24,
            80,
            TEST_SCROLLBACK_LIMIT_BYTES,
            HostTerminalTheme::default(),
            CellPixelSize { width: 9, height: 18 },
        );
        let effects = grid.process(b"\x1b[16t\x1b[18t");
        let responses = effects.pty_responses.concat();
        assert!(responses
            .windows(b"\x1b[6;18;9t".len())
            .any(|part| part == b"\x1b[6;18;9t"));
        assert!(responses
            .windows(b"\x1b[8;24;80t".len())
            .any(|part| part == b"\x1b[8;24;80t"));
    }

    #[test]
    fn tracks_cursor_shape_and_blinking() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"\x1b[5 q");
        assert_eq!(grid.screen().cursor_shape(), CursorShape::Bar);
        assert!(grid.screen().cursor_blinking());
        grid.process(b"\x1b[4 q");
        assert_eq!(grid.screen().cursor_shape(), CursorShape::Underline);
        assert!(!grid.screen().cursor_blinking());
    }

    #[test]
    fn tracks_bracketed_paste_mode() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        assert!(!grid.has_bracketed_paste());

        grid.process(b"\x1b[?2004h");
        assert!(grid.has_bracketed_paste());

        grid.process(b"\x1b[?2004l");
        assert!(!grid.has_bracketed_paste());
    }

    #[test]
    fn answers_split_default_color_query_from_host_theme() {
        let theme = HostTerminalTheme {
            background: Some(RgbColor {
                r: 40,
                g: 42,
                b: 54,
            }),
            ..HostTerminalTheme::default()
        };
        let mut grid = TermGrid::with_host_theme(1, 8, TEST_SCROLLBACK_LIMIT_BYTES, theme);
        let first = grid.process(b"\x1b]1");
        assert!(first.pty_responses.is_empty());
        let second = grid.process(b"1;?\x1b\\");
        assert_eq!(second.pty_responses.len(), 1);
        assert!(second.pty_responses[0].starts_with(b"\x1b]11;rgb:2828/2a2a/3636"));
    }

    #[test]
    fn answers_palette_query_from_host_theme() {
        let mut theme = HostTerminalTheme::default();
        theme.palette[7] = Some(RgbColor { r: 12, g: 34, b: 56 });
        let mut grid =
            TermGrid::with_host_theme(1, 8, TEST_SCROLLBACK_LIMIT_BYTES, theme);
        let queries = grid.process(b"\x1b]4;7;?\x1b\\");
        assert_eq!(queries.pty_responses.len(), 1);
        assert!(queries.pty_responses[0].starts_with(b"\x1b]4;7;rgb:0c0c/2222/3838"));
    }

    #[test]
    fn forwards_xtversion_and_xtgettcap_responses() {
        let mut grid = TermGrid::new(1, 8, TEST_SCROLLBACK_LIMIT_BYTES);
        let effects = grid.process(b"\x1b[>q\x1bP+q524742\x1b\\");
        let responses = effects.pty_responses.concat();
        assert!(responses
            .windows(b"termul 0.1.0".len())
            .any(|part| part == b"termul 0.1.0"));
        assert!(responses
            .windows(b"524742".len())
            .any(|part| part == b"524742"));
    }

    #[test]
    fn detects_batched_startup_capability_queries() {
        let theme = HostTerminalTheme {
            foreground: Some(RgbColor {
                r: 248,
                g: 248,
                b: 242,
            }),
            background: Some(RgbColor {
                r: 40,
                g: 42,
                b: 54,
            }),
            cursor: Some(RgbColor {
                r: 248,
                g: 248,
                b: 242,
            }),
            ..HostTerminalTheme::default()
        };
        let mut grid = TermGrid::with_host_theme(24, 80, TEST_SCROLLBACK_LIMIT_BYTES, theme);
        let effects = grid.process(
            b"\x1b[6n\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b[?996n\x1b[?u\x1b[c",
        );
        let responses = effects.pty_responses.concat();
        assert!(responses.windows(6).any(|part| part == b"\x1b[1;1R"));
        assert!(responses.windows(5).any(|part| part == b"\x1b[?0u"));
        assert!(responses.windows(7).any(|part| part == b"\x1b[?1;2c"));
        assert!(responses.windows(5).any(|part| part == b"\x1b]10;"));
        assert!(responses.windows(5).any(|part| part == b"\x1b]11;"));
        assert!(responses.windows(5).any(|part| part == b"\x1b]12;"));
        assert!(responses
            .windows(b"\x1b[?997;1n".len())
            .any(|part| part == b"\x1b[?997;1n"));
    }

    #[test]
    fn ghostty_viewport_scrolls_and_returns_to_bottom() {
        let mut grid = TermGrid::new(3, 20, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        grid.scroll_lines(-2);
        assert!(grid.scrollback() > 0);
        assert!(grid.screen().contents().contains("two"));
        grid.set_scrollback(0);
        assert_eq!(grid.scrollback(), 0);
        assert!(grid.screen().contents().contains("five"));
    }

    #[test]
    fn larger_byte_limit_retains_more_scrollback() {
        let mut output = String::new();
        for i in 0..1_500 {
            output.push_str(&format!("{i:04} {}\r\n", "x".repeat(70)));
        }

        let mut small = TermGrid::new(3, 80, 100_000);
        let mut large = TermGrid::new(3, 80, 10_000_000);
        small.process(output.as_bytes());
        large.process(output.as_bytes());
        small.set_scrollback(usize::MAX);
        large.set_scrollback(usize::MAX);

        assert!(large.scrollback() > small.scrollback());
    }

    #[test]
    fn resize_at_bottom_keeps_following_live_output() {
        let mut grid = TermGrid::new(3, 10, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"000000\r\n000001\r\n000002\r\n000003\r\n000004");

        grid.resize(4, 12);
        assert_eq!(grid.scrollback(), 0);
        grid.process(b"\r\n000005");

        assert_eq!(grid.scrollback(), 0);
        assert!(grid.screen().contents().contains("000005"));
    }

    #[test]
    fn resize_preserves_scrolled_offset_after_reflow() {
        let mut grid = TermGrid::new(4, 12, TEST_SCROLLBACK_LIMIT_BYTES);
        let output = (0..40)
            .map(|i| format!("{i:02}-abcdefghijklmnop\r\n"))
            .collect::<String>();
        grid.process(output.as_bytes());
        grid.set_scrollback(20);

        for (rows, cols) in [(4, 10), (4, 7), (6, 18), (3, 9), (5, 12)] {
            let before = grid.scrollback();
            grid.resize(rows, cols);
            assert_eq!(grid.scrollback(), before);
            assert!(!grid.screen().contents().trim().is_empty());
        }
    }

    #[test]
    fn resize_clamps_offset_when_scrollback_disappears() {
        let mut grid = TermGrid::new(3, 5, TEST_SCROLLBACK_LIMIT_BYTES);
        grid.process(b"abcdefghijklmnopqrstuvwxyz0123456789");
        grid.set_scrollback(usize::MAX);
        assert!(grid.scrollback() > 0);

        grid.resize(3, 80);
        assert_eq!(grid.scrollback(), 0);
        grid.process(b"\r\nnext");

        assert_eq!(grid.scrollback(), 0);
        assert!(grid.screen().contents().contains("next"));
    }
}
