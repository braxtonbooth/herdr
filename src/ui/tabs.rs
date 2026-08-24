use std::collections::HashMap;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::status::state_icon_symbol;
use super::text::{display_width_u16, truncate_end};
use super::widgets::panel_contrast_fg;
use crate::app::AppState;
use crate::config::{StatusIndicatorStyle, TabAutoNameConfig, TabStatusConfig};
use crate::detect::AgentState;
use crate::terminal::{TerminalId, TerminalState};

const MIN_TAB_WIDTH: u16 = 8;
const NEW_TAB_WIDTH: u16 = 3;
const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;

#[derive(Debug, Clone, Default)]
pub(crate) struct TabBarView {
    pub scroll: usize,
    pub tab_hit_areas: Vec<Rect>,
    pub scroll_left_hit_area: Rect,
    pub scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
}

/// Inputs the tab bar needs beyond the workspace itself. Terminal titles and agent
/// state live in `AppState`, not on `Workspace`, so they arrive by reference.
#[derive(Clone, Copy)]
pub(crate) struct TabChromeCtx<'a> {
    terminals: &'a HashMap<TerminalId, TerminalState>,
    auto_name: TabAutoNameConfig,
    auto_name_max_width: u16,
    status: TabStatusConfig,
    indicators: StatusIndicatorStyle,
}

pub(crate) fn tab_chrome_ctx(app: &AppState) -> TabChromeCtx<'_> {
    TabChromeCtx {
        terminals: &app.terminals,
        auto_name: app.tab_auto_name,
        auto_name_max_width: app.tab_auto_name_max_width,
        status: app.show_tab_status,
        indicators: app.status_indicators,
    }
}

/// One tab's rendered chrome: an optional status glyph followed by the tab name and
/// any zoom marker. Kept as parts rather than one string so the glyph can carry the
/// agent-state color while the name keeps the tab's own style.
#[derive(Debug, Clone, Default)]
pub(crate) struct TabChromeLabel {
    status: Option<TabStatusGlyph>,
    body: String,
}

#[derive(Debug, Clone, Copy)]
struct TabStatusGlyph {
    symbol: &'static str,
    state: AgentState,
    seen: bool,
}

impl TabChromeLabel {
    /// Width of everything the tab draws, so hit areas and scroll math account for
    /// the glyph the same way they already account for the zoom marker.
    fn content_width(&self) -> u16 {
        let status = self
            .status
            .map(|glyph| display_width_u16(glyph.symbol).saturating_add(1))
            .unwrap_or(0);
        status.saturating_add(display_width_u16(&self.body))
    }

    #[cfg(test)]
    fn plain_text(&self) -> String {
        match self.status {
            Some(glyph) => format!("{} {}", glyph.symbol, self.body),
            None => self.body.clone(),
        }
    }
}

fn tab_width(label: &TabChromeLabel) -> u16 {
    label.content_width().saturating_add(4).max(MIN_TAB_WIDTH)
}

/// Resolve every tab's chrome once per layout pass. `centered_tab_scroll` retries the
/// layout at each candidate scroll offset, so resolving labels per width lookup would
/// re-walk the terminal map O(tabs^2) times per frame.
pub(crate) fn tab_chrome_labels(
    ws: &crate::workspace::Workspace,
    ctx: TabChromeCtx<'_>,
) -> Vec<TabChromeLabel> {
    (0..ws.tabs.len())
        .map(|idx| tab_chrome_label(ws, idx, ctx))
        .collect()
}

fn tab_chrome_label(
    ws: &crate::workspace::Workspace,
    tab_idx: usize,
    ctx: TabChromeCtx<'_>,
) -> TabChromeLabel {
    let tab = ws.tabs.get(tab_idx);
    let name = tab_name(ws, tab_idx, ctx);
    let body = if tab.is_some_and(|tab| tab.zoomed) {
        format!("{name} Z")
    } else {
        name
    };

    let status = tab
        .filter(|_| ctx.status != TabStatusConfig::Off)
        .and_then(|tab| tab.aggregate_state(ctx.terminals))
        .filter(|(state, seen)| ctx.status.shows(*state, *seen))
        .map(|(state, seen)| TabStatusGlyph {
            symbol: state_icon_symbol(state, seen, ctx.indicators),
            state,
            seen,
        });

    TabChromeLabel { status, body }
}

/// An explicit rename always wins; the terminal title is only a better default than a
/// bare tab number.
fn tab_name(
    ws: &crate::workspace::Workspace,
    tab_idx: usize,
    ctx: TabChromeCtx<'_>,
) -> String {
    if let Some(custom) = ws.tabs.get(tab_idx).and_then(|tab| tab.custom_name.clone()) {
        return custom;
    }

    if ctx.auto_name == TabAutoNameConfig::TerminalTitle {
        if let Some(title) = ws
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.focused_terminal_title(ctx.terminals))
        {
            return truncate_end(&title, ctx.auto_name_max_width as usize);
        }
    }

    ws.tab_display_name(tab_idx)
        .unwrap_or_else(|| (tab_idx + 1).to_string())
}

fn layout_tab_hit_areas(labels: &[TabChromeLabel], area: Rect, scroll: usize) -> Vec<Rect> {
    let mut rects = vec![Rect::default(); labels.len()];
    if area.width == 0 || area.height == 0 {
        return rects;
    }

    let mut x = area.x;
    let right = area.x + area.width;
    for (idx, rect) in rects.iter_mut().enumerate().skip(scroll) {
        if x >= right {
            break;
        }
        let Some(label) = labels.get(idx) else {
            break;
        };
        let desired = tab_width(label);
        let remaining = right.saturating_sub(x);
        let width = desired.min(remaining).max(1);
        *rect = Rect::new(x, area.y, width, 1);
        x = x.saturating_add(width + 1);
    }
    rects
}

fn centered_tab_scroll(labels: &[TabChromeLabel], active_tab: usize, area: Rect) -> usize {
    let mut best_scroll = active_tab;
    let mut best_distance = u16::MAX;
    let viewport_center = area.x.saturating_mul(2).saturating_add(area.width);

    for scroll in 0..=active_tab {
        let rects = layout_tab_hit_areas(labels, area, scroll);
        let Some(active_rect) = rects.get(active_tab).copied() else {
            continue;
        };
        if active_rect.width == 0 {
            continue;
        }

        let active_center = active_rect
            .x
            .saturating_mul(2)
            .saturating_add(active_rect.width);
        let distance = active_center.abs_diff(viewport_center);
        if distance <= best_distance {
            best_distance = distance;
            best_scroll = scroll;
        }
    }

    best_scroll
}

fn trailing_tab_controls_x(tab_hit_areas: &[Rect], fallback_x: u16) -> u16 {
    tab_hit_areas
        .iter()
        .rev()
        .find(|rect| rect.width > 0)
        .map(|rect| rect.x + rect.width)
        .unwrap_or(fallback_x)
}

fn max_tab_scroll(labels: &[TabChromeLabel], area: Rect) -> usize {
    (0..labels.len())
        .find(|&scroll| {
            layout_tab_hit_areas(labels, area, scroll)
                .last()
                .is_some_and(|rect| rect.width > 0)
        })
        .unwrap_or(0)
}

pub(crate) fn compute_tab_bar_view(
    ws: &crate::workspace::Workspace,
    ctx: TabChromeCtx<'_>,
    area: Rect,
    current_scroll: usize,
    follow_active: bool,
    mouse_chrome: bool,
) -> TabBarView {
    if area.width == 0 || area.height == 0 {
        return TabBarView::default();
    }

    let labels = tab_chrome_labels(ws, ctx);
    let active_tab = ws.active_tab;

    if !mouse_chrome {
        let max_scroll = max_tab_scroll(&labels, area);
        let scroll = if follow_active {
            centered_tab_scroll(&labels, active_tab, area).min(max_scroll)
        } else {
            current_scroll.min(max_scroll)
        };
        return TabBarView {
            scroll,
            tab_hit_areas: layout_tab_hit_areas(&labels, area, scroll),
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area: Rect::default(),
        };
    }

    let area_right = area.x + area.width;
    let all_tabs_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(NEW_TAB_WIDTH),
        area.height,
    );
    let all_tabs = layout_tab_hit_areas(&labels, all_tabs_area, 0);
    let overflow = all_tabs.iter().any(|rect| rect.width == 0);
    if !overflow {
        let new_tab_x = trailing_tab_controls_x(&all_tabs, area.x);
        let new_tab_hit_area = Rect::new(
            new_tab_x,
            area.y,
            area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
            1,
        );
        return TabBarView {
            scroll: 0,
            tab_hit_areas: all_tabs,
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area,
        };
    }

    let left_hit_area = Rect::new(area.x, area.y, TAB_SCROLL_BUTTON_WIDTH.min(area.width), 1);
    let tab_area_x = left_hit_area.x + left_hit_area.width;
    let reserved_trailing_width = NEW_TAB_WIDTH.saturating_add(TAB_SCROLL_BUTTON_WIDTH);
    let tab_area_right = area_right.saturating_sub(reserved_trailing_width);
    let tab_area = Rect::new(
        tab_area_x,
        area.y,
        tab_area_right.saturating_sub(tab_area_x),
        area.height,
    );

    let max_scroll = max_tab_scroll(&labels, tab_area);
    let scroll = if follow_active {
        centered_tab_scroll(&labels, active_tab, tab_area).min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    };
    let tab_hit_areas = layout_tab_hit_areas(&labels, tab_area, scroll);
    let trailing_x = trailing_tab_controls_x(&tab_hit_areas, tab_area_x).min(tab_area_right);
    let right_hit_area = Rect::new(
        trailing_x,
        area.y,
        area_right
            .saturating_sub(trailing_x)
            .min(TAB_SCROLL_BUTTON_WIDTH),
        1,
    );
    let new_tab_x = right_hit_area.x + right_hit_area.width;
    let new_tab_hit_area = Rect::new(
        new_tab_x,
        area.y,
        area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
        1,
    );

    TabBarView {
        scroll,
        tab_hit_areas,
        scroll_left_hit_area: left_hit_area,
        scroll_right_hit_area: right_hit_area,
        new_tab_hit_area,
    }
}

fn tab_drop_indicator_x(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    insert_idx: usize,
) -> Option<u16> {
    let mut visible_tabs = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .filter(|(_, rect)| rect.width > 0);
    let first_visible = visible_tabs.clone().next()?;
    let last_visible = visible_tabs.next_back().unwrap_or(first_visible);

    if insert_idx == 0 {
        return Some(if first_visible.0 == 0 {
            first_visible.1.x
        } else {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        });
    }

    if let Some((_, rect)) = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(idx, rect)| *idx == insert_idx && rect.width > 0)
    {
        return Some(rect.x.saturating_sub(1));
    }

    if insert_idx >= ws.tabs.len() {
        return Some(if last_visible.0 + 1 >= ws.tabs.len() {
            last_visible.1.x + last_visible.1.width
        } else {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        });
    }

    None
}

pub(super) fn render_tab_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(active_ws_idx) = app.active else {
        return;
    };
    let Some(ws) = app.workspaces.get(active_ws_idx) else {
        return;
    };
    let p = &app.palette;

    frame.render_widget(
        Paragraph::new(" ".repeat(area.width as usize)).style(Style::default().bg(p.panel_bg)),
        area,
    );

    let first_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let last_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .rev()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let can_scroll_left = app.view.tab_scroll_left_hit_area.width > 0 && app.tab_scroll > 0;
    let can_scroll_right = app.view.tab_scroll_right_hit_area.width > 0
        && last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len());

    if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
        let style = if can_scroll_left {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" < ").style(style),
            app.view.tab_scroll_left_hit_area,
        );
    }

    if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
        let style = if can_scroll_right {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" > ").style(style),
            app.view.tab_scroll_right_hit_area,
        );
    }

    let labels = tab_chrome_labels(ws, tab_chrome_ctx(app));

    for (idx, tab) in ws.tabs.iter().enumerate() {
        let Some(rect) = app.view.tab_hit_areas.get(idx).copied() else {
            break;
        };
        if rect.width == 0 {
            continue;
        }
        let active = idx == ws.active_tab;
        let style = if active {
            let base = Style::default().fg(panel_contrast_fg(p)).bg(p.accent);
            if tab.is_auto_named() {
                base
            } else {
                base.add_modifier(Modifier::BOLD)
            }
        } else if tab.is_auto_named() {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(p.overlay1).bg(p.surface0)
        };
        let width = rect.width as usize;
        let label = labels.get(idx).cloned().unwrap_or_default();
        // Pad the body to fill the tab so the active-tab background covers the cell,
        // reserving the leading space plus whatever the status glyph occupies.
        let reserved = 1 + label
            .status
            .map(|glyph| glyph.symbol.chars().count() + 1)
            .unwrap_or(0);
        let body = format!(
            "{:width$}",
            label.body,
            width = width.saturating_sub(reserved)
        );
        let line = match label.status {
            Some(glyph) => Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    glyph.symbol,
                    style.fg(super::status::state_label_color(glyph.state, glyph.seen, p)),
                ),
                Span::raw(" "),
                Span::raw(body),
            ]),
            None => Line::from(vec![Span::raw(" "), Span::raw(body)]),
        };
        frame.render_widget(Paragraph::new(line).style(style), rect);
    }

    if let Some(crate::app::state::DragState {
        target:
            crate::app::state::DragTarget::TabReorder {
                ws_idx,
                insert_idx: Some(insert_idx),
                ..
            },
    }) = &app.drag
    {
        if *ws_idx == active_ws_idx {
            if let Some(x) = tab_drop_indicator_x(app, ws, *insert_idx) {
                frame.buffer_mut()[(x.min(area.x + area.width.saturating_sub(1)), area.y)]
                    .set_symbol("│")
                    .set_style(Style::default().fg(p.accent));
            }
        }
    }

    if app.mouse_capture && app.view.new_tab_hit_area.width > 0 {
        frame.render_widget(
            Paragraph::new(" + ").style(Style::default().fg(p.overlay1)),
            app.view.new_tab_hit_area,
        );
    }

    if first_visible_idx.is_some_and(|idx| idx > 0) {
        let x = if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        } else {
            area.x
        };
        if x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(p.overlay0));
        }
    }
    if last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len()) {
        let x = if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        } else {
            area.x + area.width.saturating_sub(1)
        };
        if x >= area.x && x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(p.overlay0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Width-only assertions need a context but no live terminals.
    fn test_ctx(terminals: &HashMap<TerminalId, TerminalState>) -> TabChromeCtx<'_> {
        TabChromeCtx {
            terminals,
            auto_name: TabAutoNameConfig::Number,
            auto_name_max_width: crate::config::DEFAULT_TAB_AUTO_NAME_MAX_WIDTH,
            status: TabStatusConfig::Off,
            indicators: StatusIndicatorStyle::Dots,
        }
    }

    /// Attach a terminal title and agent state to tab 0's root pane.
    fn seed_focused_terminal(app: &mut AppState, title: Option<&str>, state: AgentState) {
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.terminal_title = title.map(str::to_string);
        terminal.state = state;
    }

    fn render_row(app: &AppState) -> String {
        let backend = TestBackend::new(app.view.tab_bar_rect.width, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(app, frame, app.view.tab_bar_rect))
            .unwrap();
        buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0)
    }

    fn lay_out(app: &mut AppState) {
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            tab_chrome_ctx(app),
            app.view.tab_bar_rect,
            0,
            true,
            false,
        );
        app.view.tab_hit_areas = view.tab_hit_areas;
    }

    #[test]
    fn tab_bar_marks_zoomed_tabs_without_renaming_them() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        let custom_tab = ws.test_add_tab(Some("test"));
        ws.tabs[custom_tab].zoomed = true;

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        lay_out(&mut app);

        let row = render_row(&app);
        assert!(row.contains(" 1 Z"), "tab row: {row:?}");
        assert!(row.contains(" test Z"), "tab row: {row:?}");
        assert_eq!(app.workspaces[0].tab_display_name(0).as_deref(), Some("1"));
        assert_eq!(
            app.workspaces[0].tab_display_name(custom_tab).as_deref(),
            Some("test")
        );
    }

    #[test]
    fn active_auto_named_tab_keeps_readable_weight() {
        let mut app = AppState::test_new();
        let ws = Workspace::test_new("test");

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        lay_out(&mut app);

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let tab_rect = app.view.tab_hit_areas[0];
        let style = terminal.backend().buffer()[(tab_rect.x + 1, tab_rect.y)].style();

        assert_eq!(style.bg, Some(app.palette.accent));
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn zoom_marker_counts_toward_tab_width() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("abcdefgh".into());
        ws.tabs[0].zoomed = true;
        let terminals = HashMap::new();

        assert_eq!(tab_width(&tab_chrome_label(&ws, 0, test_ctx(&terminals))), 14);
    }

    #[test]
    fn tab_width_uses_display_width_for_cjk_labels() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());
        let terminals = HashMap::new();

        assert_eq!(
            tab_width(&tab_chrome_label(&ws, 0, test_ctx(&terminals))),
            display_width_u16("提交 herdr 的反馈") + 4
        );
    }

    #[test]
    fn tab_bar_renders_trailing_cjk_character() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());

        app.active = Some(0);
        app.workspaces = vec![ws];
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        lay_out(&mut app);

        let row = render_row(&app);
        assert!(row.contains('馈'), "tab row: {row:?}");
    }

    #[test]
    fn terminal_title_auto_name_replaces_the_tab_number() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.tab_auto_name = TabAutoNameConfig::TerminalTitle;
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        seed_focused_terminal(&mut app, Some("✳ Fix the parser"), AgentState::Working);
        lay_out(&mut app);

        let row = render_row(&app);
        assert!(row.contains("Fix the parser"), "tab row: {row:?}");
        // The animated prefix must not reach the label; it changes every frame and
        // would reflow the whole bar.
        assert!(!row.contains('✳'), "tab row: {row:?}");
        // The socket API and rename dialog keep seeing the tab's own identity.
        assert_eq!(app.workspaces[0].tab_display_name(0).as_deref(), Some("1"));
    }

    #[test]
    fn explicit_tab_name_wins_over_terminal_title() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.tab_auto_name = TabAutoNameConfig::TerminalTitle;
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        app.workspaces[0].tabs[0].set_custom_name("review".into());
        seed_focused_terminal(&mut app, Some("Fix the parser"), AgentState::Working);
        lay_out(&mut app);

        let row = render_row(&app);
        assert!(row.contains("review"), "tab row: {row:?}");
        assert!(!row.contains("Fix the parser"), "tab row: {row:?}");
    }

    #[test]
    fn terminal_title_auto_name_respects_max_width() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.tab_auto_name = TabAutoNameConfig::TerminalTitle;
        app.tab_auto_name_max_width = 10;
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        seed_focused_terminal(
            &mut app,
            Some("⠋ a very long agent task title"),
            AgentState::Working,
        );
        lay_out(&mut app);

        let row = render_row(&app);
        assert!(row.contains('…'), "tab row: {row:?}");
        assert!(!row.contains("task title"), "tab row: {row:?}");
        // One tab at width 10 must not consume a 40-column bar.
        assert!(app.view.tab_hit_areas[0].width <= 15);
    }

    #[test]
    fn custom_tab_name_is_never_truncated() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("a very long explicit tab name".into());
        let terminals = HashMap::new();
        let mut ctx = test_ctx(&terminals);
        ctx.auto_name = TabAutoNameConfig::TerminalTitle;
        ctx.auto_name_max_width = 10;

        assert_eq!(
            tab_chrome_label(&ws, 0, ctx).body,
            "a very long explicit tab name"
        );
    }

    #[test]
    fn status_glyph_counts_toward_tab_width() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("abcdefgh".into());
        let terminals = HashMap::new();
        let ctx = test_ctx(&terminals);

        let without = tab_width(&tab_chrome_label(&ws, 0, ctx));
        let label = TabChromeLabel {
            status: Some(TabStatusGlyph {
                symbol: "●",
                state: AgentState::Blocked,
                seen: true,
            }),
            body: "abcdefgh".into(),
        };

        // Glyph plus its separating space widen the tab by exactly two columns.
        assert_eq!(tab_width(&label), without + 2);
        assert_eq!(label.plain_text(), "● abcdefgh");
    }

    #[test]
    fn attention_status_shows_blocked_and_hides_working() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.show_tab_status = TabStatusConfig::Attention;
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);

        seed_focused_terminal(&mut app, None, AgentState::Working);
        lay_out(&mut app);
        let working = render_row(&app);
        assert!(!working.contains('●'), "working tab row: {working:?}");

        seed_focused_terminal(&mut app, None, AgentState::Blocked);
        lay_out(&mut app);
        let blocked = render_row(&app);
        assert!(blocked.contains('●'), "blocked tab row: {blocked:?}");
    }

    #[test]
    fn status_off_leaves_the_tab_bar_unchanged() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        seed_focused_terminal(&mut app, Some("Fix the parser"), AgentState::Blocked);
        lay_out(&mut app);

        let row = render_row(&app);
        assert_eq!(row, " 1");
    }
}
