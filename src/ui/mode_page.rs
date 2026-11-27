use std::borrow::Cow;

use anyhow::Result;
use eframe::egui::{
    self, Align, Color32, Context, FontId, Layout, Painter, Pos2, Rect, RichText, Sense, Stroke,
    TextureHandle, Ui, WidgetInfo, WidgetType, vec2,
};
use uuid::Uuid;

use super::{
    DeckX,
    deck_face::{self, DeckFace, FaceControl},
    glyph,
    mode_editor::{AgentState, Assignments, Editor},
};
use crate::{
    actions::{self, ActionKind, PreparedAction},
    agent_layout::{MainDial, Preset},
    deck_model::AKP03E,
    glyphs::TitleStyle,
    profile::{Control, DISPLAY_KEYS},
    theme::{
        self, BODY, CANVAS, DETAIL, DialogAction, ERROR, GAP, INSET, MUTED, PAGE_INSET, ROW_TITLE,
        STACK_GAP, TEXT, WELL,
    },
    usage::{self, Report},
};

pub(super) enum OpenTarget<'a> {
    Chat(Uuid),
    Bot(&'a str),
    ClaudeChat(Uuid),
}

impl OpenTarget<'_> {
    pub(super) fn run(self) -> Result<()> {
        match self {
            Self::Chat(id) => PreparedAction::CodexAgent(id).run(),
            Self::Bot(id) => actions::open_grok_agent(id),
            Self::ClaudeChat(id) => PreparedAction::Claude(crate::claude::Target::Chat(id)).run(),
        }
    }
}

/// Adapts a provider's data to the common layout page and editor.
pub(super) trait ModePage: Clone {
    type Layout: Assignments;
    const OPEN_LABEL: &'static str;

    fn active(app: &DeckX) -> Option<&Self>;
    fn state(app: &DeckX) -> &AgentState<Self::Layout>;
    fn state_mut(app: &mut DeckX) -> &mut AgentState<Self::Layout>;

    fn saved(app: &DeckX) -> &Self::Layout {
        &Self::state(app).saved
    }

    fn editor(app: &mut DeckX) -> &mut Option<Editor<Self::Layout>> {
        &mut Self::state_mut(app).editor
    }

    fn catalog<'a>(&'a self, app: &'a DeckX) -> &'a <Self::Layout as Assignments>::Catalog;
    fn save(app: &mut DeckX, draft: Self::Layout) -> Result<()>;
    fn errors<'a>(&'a self, app: &'a DeckX) -> impl Iterator<Item = &'a str>;
    fn style_controls(_ui: &mut Ui, _app: &mut DeckX) {}
    fn describe<'a>(&'a self, app: &'a DeckX, control: Control) -> ControlDisplay<'a>;
    fn paint_lcd(&self, app: &DeckX, ui: &mut Ui, key: KeyFace<'_>, display: &ControlDisplay<'_>);
    fn selected(&self) -> Option<usize>;
    fn summary(&self) -> Summary;
    fn open(
        app: &mut DeckX,
        index: usize,
        open: impl FnOnce(OpenTarget<'_>) -> Result<()>,
    ) -> Result<()>;
}

pub(super) struct Summary {
    pub agents: (&'static str, Cow<'static, str>),
    pub dial: (&'static str, &'static str),
}

pub(super) fn show<M: ModePage>(
    app: &mut DeckX,
    ctx: &Context,
    mut open: impl FnMut(OpenTarget<'_>) -> Result<()>,
) {
    let Some(mode) = M::active(app).cloned() else {
        return;
    };
    let deck_width = DeckFace::mode_width(&AKP03E, ctx.available_rect().height());
    egui::CentralPanel::default()
        .frame(theme::frame(CANVAS, 0))
        .show(ctx, |ui| {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = vec2(GAP, STACK_GAP);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(PAGE_INSET);
                        let width = (ui.available_width() - 56.0).min(672.0);
                        theme::centered_column(ui, width, |ui| {
                            content(ui, app, &mode, deck_width, &mut open);
                        });
                        ui.add_space(PAGE_INSET);
                    });
            });
        });
    if let Some(mut editor) = M::editor(app).take() {
        match editor.show(ctx, mode.catalog(app)) {
            DialogAction::Cancel => return,
            DialogAction::Confirm => match M::save(app, editor.draft.clone()) {
                Ok(()) => return,
                Err(error) => editor.error = Some(format!("{error:#}")),
            },
            DialogAction::None => {}
        }
        *M::editor(app) = Some(editor);
    } else if app.usage_details
        && let Some((_, provider)) = app.mode.active.usage_key()
    {
        app.usage_details = usage_dialog(ctx, provider, app.usage.readings.get(provider));
    }
}

fn content<M: ModePage>(
    ui: &mut Ui,
    app: &mut DeckX,
    mode: &M,
    deck_width: f32,
    open: &mut impl FnMut(OpenTarget<'_>) -> Result<()>,
) {
    brightness(ui, app);
    for error in app
        .error
        .then_some(app.message.as_str())
        .into_iter()
        .chain(mode.errors(app))
    {
        ui.label(RichText::new(error).size(DETAIL).color(ERROR));
    }
    theme::section_header(ui, "Layout", |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::text_button(ui, "Reset layout…", 0.0, false, true).clicked() {
                *M::editor(app) = Some(Editor::new(None, M::saved(app).defaults()));
            }
            ui.add_space(GAP);
            M::style_controls(ui, app);
        });
    });
    let event = surface()
        .show(ui, |ui| {
            ui.add_space(INSET);
            let mut event = ui
                .vertical_centered(|ui| deck(ui, app, mode, deck_width))
                .inner;
            ui.add_space(INSET);
            ui.separator();
            if let Some(index) = mode.selected() {
                let display = mode.describe(app, Control::Button(index));
                let name = Control::Button(index).label();
                let detail = display
                    .detail
                    .map_or(name.clone(), |detail| format!("{name} · {detail}"));
                theme::detail_row(ui, display.label, &detail, 118.0, |ui| {
                    if theme::text_button(ui, M::OPEN_LABEL, 0.0, false, display.can_open).clicked()
                    {
                        event = Some(PreviewEvent::Open(index));
                    }
                });
            } else {
                theme::detail_row(
                    ui,
                    "Assignments",
                    "Click a key or dial to edit. Right-click for more options.",
                    0.0,
                    |_| {},
                );
            }
            event
        })
        .inner;
    match event {
        Some(PreviewEvent::Edit(control)) => {
            *M::editor(app) = Some(Editor::new(Some(control), M::saved(app).clone()));
        }
        Some(PreviewEvent::Open(index)) => {
            if let Err(error) = M::open(app, index, open) {
                app.report(format!("{error:#}"), true);
            }
        }
        Some(PreviewEvent::Usage) => app.usage_details = true,
        None => {}
    }
    let preset = M::saved(app).preset();
    let main_dial = app.saved.agent_main_dial;
    let (next, next_dial) = options(
        ui,
        mode.summary(),
        preset,
        M::state(app).load_error.is_none(),
        main_dial,
        app.path.is_ok() && !app.unreadable,
    );
    if next != preset {
        let mut draft = M::saved(app).clone();
        draft.set_preset(next);
        match M::save(app, draft) {
            Ok(()) => app.sync_usage(),
            Err(error) => app.report(format!("{error:#}"), true),
        }
        ui.ctx().request_repaint();
    }
    if main_dial != next_dial {
        match app.save_agent_main_dial(next_dial) {
            Ok(()) => app.report("Main dial saved", false),
            Err(error) => app.report(format!("{error:#}"), true),
        }
        ui.ctx().request_repaint();
    }
}

pub(super) struct ControlDisplay<'a> {
    pub label: &'a str,
    pub detail: Option<&'a str>,
    pub icon: ControlIcon,
    pub highlight: bool,
    pub can_open: bool,
}

impl<'a> ControlDisplay<'a> {
    pub(super) fn binding(label: &'a str, icon: ControlIcon) -> Self {
        Self {
            label,
            detail: None,
            icon,
            highlight: false,
            can_open: false,
        }
    }
}

pub(super) struct KeyFace<'a> {
    pub index: usize,
    pub rect: Rect,
    pub painter: &'a Painter,
    pub id: egui::Id,
}

#[derive(Clone)]
struct TitleTexture {
    title: String,
    style: TitleStyle,
    texture: TextureHandle,
}

#[derive(Clone)]
struct UsageTexture {
    face: usage::KeyFace,
    border: Color32,
    texture: TextureHandle,
}

impl KeyFace<'_> {
    pub(super) fn fill(&self, color: Color32) {
        self.painter
            .rect_filled(self.rect, deck_face::key_radius(self.rect), color);
    }

    pub(super) fn command(&self, icon: ControlIcon) {
        self.fill(WELL);
        icon.paint(self.painter, self.rect.center(), 19.0);
    }

    pub(super) fn title(&self, title: &str, style: TitleStyle) {
        let ctx = self.painter.ctx();
        let id = self.id.with("title-texture");
        let cached = ctx.data(|data| data.get_temp::<TitleTexture>(id));
        let texture = if let Some(cached) = cached
            && cached.title == title
            && cached.style == style
        {
            cached.texture
        } else {
            let tile = style.render(title);
            let texture = ctx.load_texture(
                "Chat title",
                deck_face::lcd_image(&tile),
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|data| {
                data.insert_temp(
                    id,
                    TitleTexture {
                        title: title.to_owned(),
                        style,
                        texture: texture.clone(),
                    },
                );
            });
            texture
        };
        self.paint_tile(&texture);
    }

    fn usage(&self, report: Report, border: Color32) {
        let ctx = self.painter.ctx();
        let id = self.id.with("usage-texture");
        let face = usage::KeyFace::from_report(report);
        let cached = ctx.data(|data| data.get_temp::<UsageTexture>(id));
        let texture = if let Some(cached) = cached
            && cached.face == face
            && cached.border == border
        {
            cached.texture
        } else {
            let tile = face.render(border);
            let texture = ctx.load_texture(
                "Usage limits",
                deck_face::lcd_image(&tile),
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|data| {
                data.insert_temp(
                    id,
                    UsageTexture {
                        face,
                        border,
                        texture: texture.clone(),
                    },
                )
            });
            texture
        };
        self.paint_tile(&texture);
    }

    fn paint_tile(&self, texture: &TextureHandle) {
        self.painter.add(
            egui::epaint::RectShape::filled(
                self.rect,
                deck_face::key_radius(self.rect),
                Color32::WHITE,
            )
            .with_texture(
                texture.id(),
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
            ),
        );
    }
}

enum PreviewEvent {
    Edit(Control),
    Open(usize),
    Usage,
}

fn deck<M: ModePage>(ui: &mut Ui, app: &DeckX, mode: &M, deck_width: f32) -> Option<PreviewEvent> {
    let face = DeckFace::allocate(ui, &AKP03E, (ui.available_width() - 48.0).min(deck_width));
    let painter = ui.painter().clone();
    face.paint(&painter);
    let mut event = None;
    let usage = app
        .mode
        .active
        .usage_key()
        .map(|(index, provider)| (index, app.usage.readings.get(provider)));
    for shape in face.controls() {
        let control = shape.control();
        if let FaceControl::Lcd { index, rect } = shape
            && let Some((_, report)) = usage.filter(|(reserved, _)| *reserved == index)
        {
            let response = ui
                .interact(rect, ui.id().with("usage-key"), Sense::click())
                .on_hover_text("View usage limits");
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "Usage limits"));
            if response.clicked() {
                event = Some(PreviewEvent::Usage);
            }
            KeyFace {
                index,
                rect,
                painter: &painter,
                id: response.id,
            }
            .usage(report, deck_face::control_border(&response, false));
            continue;
        }
        let display = mode.describe(app, control);
        let mode_switch = app
            .saved
            .agent_main_dial
            .mode_turn(control)
            .and_then(|turn| app.view().cycle_agent(turn));
        let response = ui
            .interact(shape.rect(), ui.id().with(control), Sense::click())
            .on_hover_ui(|ui| {
                if let Some(next) = mode_switch {
                    ui.label(
                        RichText::new(format!("Switch to {}", next.label()))
                            .font(theme::bold(BODY)),
                    );
                    ui.label(
                        RichText::new("Click to edit saved assignment")
                            .size(DETAIL)
                            .color(MUTED),
                    );
                } else {
                    ui.label(RichText::new(display.label).font(theme::bold(BODY)));
                    if let Some(detail) = display.detail {
                        ui.label(RichText::new(detail).size(DETAIL).color(MUTED));
                    }
                    ui.label(RichText::new("Click to edit").size(DETAIL).color(MUTED));
                }
            });
        response.widget_info(|| {
            WidgetInfo::labeled(
                WidgetType::Button,
                true,
                format!("Edit {} · {}", control.label(), display.label),
            )
        });
        if response.clicked() {
            event = Some(PreviewEvent::Edit(control));
        }
        if response.secondary_clicked()
            || response.clicked() && ui.input(|input| input.pointer.any_released())
        {
            response.surrender_focus();
        }
        theme::context_menu(&response, |ui| {
            if theme::menu_item(ui, "Edit assignment", true) {
                event = Some(PreviewEvent::Edit(control));
            }
            if let Control::Button(index) = control
                && display.can_open
                && theme::menu_item(ui, M::OPEN_LABEL, true)
            {
                event = Some(PreviewEvent::Open(index));
            }
        });
        let border = deck_face::control_border(&response, display.highlight);
        if let FaceControl::Lcd { index, rect } = shape {
            mode.paint_lcd(
                app,
                ui,
                KeyFace {
                    index,
                    rect,
                    painter: &painter,
                    id: response.id,
                },
                &display,
            );
        }
        shape.paint(&painter, border);
        if let FaceControl::Round { center, .. } = shape {
            display.icon.paint(&painter, center, 17.0);
        }
    }
    event
}

#[derive(Clone, Copy, Default)]
pub(super) enum ControlIcon {
    #[default]
    None,
    Add,
    Open,
    Exit,
    Search,
    Previous,
    Next,
    Action(ActionKind),
}

impl ControlIcon {
    pub(super) fn action(kind: ActionKind) -> Self {
        match kind {
            ActionKind::CodexNewChat => Self::Add,
            ActionKind::CodexOpenChat => Self::Open,
            _ => Self::Action(kind),
        }
    }

    fn paint(self, painter: &Painter, center: Pos2, font_size: f32) {
        let grid = 2.0 * painter.ctx().pixels_per_point();
        let center = egui::pos2(
            (center.x * grid).round() / grid,
            (center.y * grid).round() / grid,
        );
        let stroke = Stroke::new(1.45, TEXT);
        let line = |(ax, ay), (bx, by)| {
            painter.line_segment([center + vec2(ax, ay), center + vec2(bx, by)], stroke);
        };
        match self {
            Self::None => {}
            Self::Add => {
                line((-4.0, 0.0), (4.0, 0.0));
                line((0.0, -4.0), (0.0, 4.0));
            }
            Self::Open => {
                line((-4.0, 4.0), (4.0, -4.0));
                line((-0.4, -4.0), (4.0, -4.0));
                line((4.0, -4.0), (4.0, 0.4));
            }
            Self::Exit => {
                line((-3.2, -3.2), (3.2, 3.2));
                line((-3.2, 3.2), (3.2, -3.2));
            }
            Self::Search => {
                painter.circle_stroke(center - vec2(1.25, 1.25), 3.0, stroke);
                line((0.9, 0.9), (4.5, 4.5));
            }
            Self::Previous | Self::Next => {
                theme::chevron(painter, center, matches!(self, Self::Next), TEXT);
            }
            Self::Action(kind) => {
                theme::centered_text(
                    painter,
                    center,
                    glyph(kind),
                    FontId::proportional(font_size),
                    TEXT,
                );
            }
        }
    }
}

fn brightness(ui: &mut Ui, app: &mut DeckX) {
    surface().show(ui, |ui| {
        let mut brightness = app.brightness_drag.unwrap_or(app.saved.brightness);
        let mut slider = None;
        theme::detail_row(
            ui,
            "Brightness",
            "Adjusts the brightness of the deck’s LCD keys",
            theme::brightness_width(ui),
            |ui| slider = Some(theme::brightness_slider(ui, &mut brightness)),
        );
        let Some(slider) = slider else {
            return;
        };
        // Show the value on the deck while dragging, and write the workspace once at the end.
        if slider.dragged() && slider.changed() {
            app.preview_brightness(brightness);
        } else if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
            match app.save_brightness(brightness) {
                Ok(()) => app.report("Brightness saved", false),
                Err(error) => app.report(format!("{error:#}"), true),
            }
            ui.ctx().request_repaint();
        }
    });
}

pub(super) fn surface() -> egui::Frame {
    theme::card(theme::PANEL, 0)
}

fn options(
    ui: &mut Ui,
    summary: Summary,
    preset: Preset,
    editable: bool,
    main_dial: MainDial,
    dial_editable: bool,
) -> (Preset, MainDial) {
    theme::section_header(ui, "Options", |_| {});
    let mut next = preset;
    let mut next_dial = main_dial;
    let usage_label = format!("{} keys + usage", DISPLAY_KEYS - 1);
    let label = |choice| match choice {
        Preset::Standard => summary.agents.1.as_ref(),
        Preset::UsageLimits => usage_label.as_str(),
    };
    let description = match preset {
        Preset::Standard => summary.agents.0,
        Preset::UsageLimits => "Show usage limits on the last LCD key",
    };
    let dial_label = |choice| match choice {
        MainDial::Assignments => summary.dial.1,
        MainDial::SwitchModes => "Switch modes",
    };
    let dial_description = match main_dial {
        MainDial::Assignments => summary.dial.0,
        MainDial::SwitchModes => "Turn to cycle Codex, Grok Bot, and Claude",
    };
    surface().show(ui, |ui| {
        theme::detail_row(ui, "Agent keys", description, 184.0, |ui| {
            ui.add_enabled_ui(editable, |ui| {
                theme::select(ui, "agent-keys-preset", label(preset), 178.0, |ui| {
                    for choice in Preset::ALL {
                        theme::select_value(ui, &mut next, choice, label(choice));
                    }
                });
            });
        });
        ui.separator();
        theme::detail_row(ui, "Main dial", dial_description, 184.0, |ui| {
            ui.add_enabled_ui(dial_editable, |ui| {
                theme::select(ui, "agent-main-dial", dial_label(main_dial), 178.0, |ui| {
                    for choice in [MainDial::Assignments, MainDial::SwitchModes] {
                        theme::select_value(ui, &mut next_dial, choice, dial_label(choice));
                    }
                });
            });
        });
    });
    (next, next_dial)
}

fn usage_dialog(ctx: &Context, provider: usage::Provider, report: Report) -> bool {
    let response = theme::modal(ctx, "usage-limits", |ui| {
        theme::dialog_title(ui, "Usage limits");
        match report {
            Report::Waiting => {
                ui.label("Reading usage limits…");
            }
            Report::Unavailable => {
                ui.label("Usage data unavailable");
                ui.label(
                    RichText::new(provider.refresh_hint())
                        .size(BODY)
                        .color(MUTED),
                );
            }
            Report::Available(reading) => {
                let now = usage::now_ms();
                let age = now.saturating_sub(reading.observed_at) / 60_000;
                let age = match age {
                    0 => "just now".into(),
                    minutes @ 1..60 => format!("{minutes} min ago"),
                    minutes @ 60..1440 => format!("{}h {}m ago", minutes / 60, minutes % 60),
                    minutes => format!("{}d ago", minutes / 1440),
                };
                ui.label(
                    RichText::new(format!("{} · {age}", provider.source_label()))
                        .size(DETAIL)
                        .color(MUTED),
                );
                for window in reading.windows.into_iter().flatten() {
                    let reset = window.resets_at.map(|at| {
                        let minutes = at.saturating_sub(now).div_ceil(60_000);
                        if minutes >= 60 {
                            format!("Resets in {}h {}m", minutes / 60, minutes % 60)
                        } else {
                            format!("Resets in {minutes}m")
                        }
                    });
                    theme::detail_row(
                        ui,
                        &window.label(),
                        reset.as_deref().unwrap_or(""),
                        92.0,
                        |ui| {
                            ui.label(
                                RichText::new(format!("{}% left", window.remaining))
                                    .font(theme::bold(ROW_TITLE)),
                            );
                        },
                    );
                }
                if reading.is_cached(now) {
                    ui.label(
                        RichText::new(provider.refresh_hint())
                            .size(DETAIL)
                            .color(MUTED),
                    );
                }
            }
        }
        theme::right_aligned_row(ui, theme::FIELD_HEIGHT, |ui| {
            theme::action_button(ui, "Close", false, true).clicked()
        })
    });
    !response.inner && !response.should_close()
}
