use anyhow::Result;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, RichText, Ui, vec2};

use super::{
    DeckX,
    mode_editor::{AgentState, Assignments, Stored, assignment_picker},
    mode_page::{ControlDisplay, ControlIcon, KeyFace, ModePage, OpenTarget, Summary},
};
use crate::{
    actions::Action,
    grok::{Binding, Bindings, Bot, Mode, key},
    profile::{Control, DISPLAY_KEYS, Direction},
    runtime::Request,
    theme::{self, ROW_TITLE, TEXT, WELL},
};

impl Assignments for Bindings {
    type Binding = Binding;
    type Catalog = [Bot];
    const NAME: &'static str = "Grok Bot";

    fn binding(&self, control: Control) -> &Binding {
        self.binding(control)
    }

    fn binding_mut(&mut self, control: Control) -> &mut Binding {
        self.binding_mut(control)
    }

    fn preset(&self) -> crate::agent_layout::Preset {
        self.preset
    }

    fn set_preset(&mut self, preset: crate::agent_layout::Preset) {
        self.preset = preset;
    }

    fn choices(control: Control) -> impl Iterator<Item = Binding> {
        std::iter::once(Binding::None)
            .chain(
                control
                    .is_lcd()
                    .then_some([Binding::MainBot, Binding::RecentBot])
                    .into_iter()
                    .flatten(),
            )
            .chain(Binding::COMMANDS)
            .chain(std::iter::once(Binding::PinnedBot {
                id: String::new(),
                label: String::new(),
            }))
    }

    fn label(binding: &Binding, _control: Control) -> &str {
        match binding {
            Binding::PinnedBot { .. } => "Open a specific bot",
            _ => binding.label(),
        }
    }

    fn same_choice(current: &Binding, choice: &Binding) -> bool {
        matches!(
            (current, choice),
            (Binding::PinnedBot { .. }, Binding::PinnedBot { .. })
        ) || current == choice
    }

    fn from_action(action: Action) -> Binding {
        Binding::Action(action)
    }

    fn action(binding: &mut Binding) -> Option<&mut Action> {
        match binding {
            Binding::Action(action) => Some(action),
            _ => None,
        }
    }

    fn details(
        ui: &mut Ui,
        _control: Control,
        binding: &mut Binding,
        bots: &[Bot],
        search: &mut String,
    ) {
        let Binding::PinnedBot { id, label } = binding else {
            return;
        };
        if let Some(index) = assignment_picker(
            ui,
            search,
            "Search bots",
            if bots.is_empty() {
                "Open Grok Bot to load your bots"
            } else {
                "No matching bots"
            },
            bots.iter().position(|bot| bot.id == *id),
            bots.iter()
                .enumerate()
                .map(|(index, bot)| (index, bot.label())),
        ) {
            id.clone_from(&bots[index].id);
            *label = bots[index].label().to_owned();
        }
    }
}

impl Stored for Bindings {
    fn file_path(profile: &std::path::Path) -> std::path::PathBuf {
        Self::path(profile)
    }

    fn read(path: &std::path::Path) -> Result<Self> {
        Self::load(path)
    }

    fn write(&self, path: &std::path::Path, previous: &Self) -> Result<()> {
        self.save(path, previous)
    }
}

impl ModePage for Mode {
    type Layout = Bindings;
    const OPEN_LABEL: &'static str = "Open bot";

    fn active(app: &DeckX) -> Option<&Self> {
        if let crate::runtime::Active::Grok(mode) = &app.mode.active {
            Some(mode)
        } else {
            None
        }
    }

    fn state(app: &DeckX) -> &AgentState<Bindings> {
        &app.grok_agent
    }

    fn state_mut(app: &mut DeckX) -> &mut AgentState<Bindings> {
        &mut app.grok_agent
    }

    fn catalog<'a>(&'a self, _app: &'a DeckX) -> &'a [Bot] {
        &self.catalog
    }

    fn save(app: &mut DeckX, draft: Bindings) -> Result<()> {
        app.save_grok_bindings(draft)
    }

    fn errors<'a>(&'a self, app: &'a DeckX) -> impl Iterator<Item = &'a str> {
        [
            self.error.as_deref(),
            self.avatar_error.as_deref(),
            app.grok_agent.load_error.as_deref(),
        ]
        .into_iter()
        .flatten()
    }

    fn style_controls(ui: &mut Ui, _app: &mut DeckX) {
        ui.label(RichText::new("Classic").size(ROW_TITLE));
    }

    fn describe<'a>(&'a self, _app: &'a DeckX, control: Control) -> ControlDisplay<'a> {
        if let Control::Button(index) = control
            && let Some(bot) = self.layout.bot(index)
        {
            let detail = match (self.layout.is_main(index), bot.unread()) {
                (true, true) => "Main bot · Unread",
                (true, false) => "Main bot · Read",
                (false, true) => "Unread",
                (false, false) => "Read",
            };
            return ControlDisplay {
                label: bot.label(),
                detail: Some(detail),
                icon: ControlIcon::None,
                highlight: self.highlight_selection && self.selected == Some(index),
                can_open: true,
            };
        }
        let binding = self.bindings.binding(control);
        let mut display = ControlDisplay::binding(binding.label(), binding_icon(binding));
        if *binding == Binding::MainBot {
            display.detail = Some("Open Grok Bot to identify the main bot");
        }
        display
    }

    fn paint_lcd(&self, app: &DeckX, ui: &mut Ui, face: KeyFace<'_>, display: &ControlDisplay<'_>) {
        let Some(bot) = self.layout.bot(face.index) else {
            face.command(display.icon);
            return;
        };
        face.fill(WELL);
        let main = self.layout.is_main(face.index);
        let preview = app.grok_previews[face.index].as_ref();
        let content = face.painter.with_clip_rect(face.rect.shrink(2.0));
        let at = |x, y| face.rect.min + vec2(x * face.rect.width(), y * face.rect.height());
        if let Some(preview) = preview {
            let rect = Rect::from_center_size(
                at(0.5, key::AVATAR_CENTER_Y),
                face.rect.size() * key::AVATAR_SCALE,
            );
            content.image(
                preview.texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let job = key::caption_job(bot.label(), face.rect.width());
        let caption = ui.fonts_mut(|fonts| fonts.layout_job(job));
        theme::centered_galley(
            &content,
            at(0.5, key::caption_center(preview.is_some())),
            caption,
            TEXT,
        );
        if bot.unread() {
            let [x, y] = key::unread_center(preview.is_some() && !main);
            let [r, g, b] = key::UNREAD_COLOR;
            content.circle_filled(
                at(x, y),
                face.rect.width() * (key::UNREAD_RADIUS + key::UNREAD_OUTLINE),
                WELL,
            );
            content.circle_filled(
                at(x, y),
                face.rect.width() * key::UNREAD_RADIUS,
                Color32::from_rgb(r, g, b),
            );
        }
        if main {
            let [x, y] = key::STAR_CENTER;
            paint_star(&content, at(x, y), face.rect.width());
        }
    }

    fn selected(&self) -> Option<usize> {
        self.selected
            .filter(|&index| self.layout.bot(index).is_some())
    }

    fn summary(&self) -> Summary {
        let defaults = Bindings::default();
        let default_for = |control| self.bindings.binding(control) == defaults.binding(control);
        Summary {
            agents: if (0..DISPLAY_KEYS).map(Control::Button).all(default_for) {
                (
                    "Shows the main bot and five most recent bots",
                    "Main bot + recent bots".into(),
                )
            } else {
                (
                    "Choose which LCD keys follow bots or run actions",
                    "Custom assignments".into(),
                )
            },
            dial: if Direction::ALL
                .into_iter()
                .all(|direction| default_for(Control::Dial(0, direction)))
            {
                ("Turn to select a bot and press to open it", "Bot selection")
            } else {
                ("Press and turn assignments", "Custom assignments")
            },
        }
    }

    fn open(
        app: &mut DeckX,
        index: usize,
        open: impl FnOnce(OpenTarget<'_>) -> Result<()>,
    ) -> Result<()> {
        let crate::runtime::Active::Grok(mode) = &mut app.mode.active else {
            return Ok(());
        };
        let previous = mode.selection();
        let result = mode.activate_key(index, |id| open(OpenTarget::Bot(id)));
        let (id, highlight) = mode.selection();
        if previous != (id.clone(), highlight)
            && let Some(session) = &app.session
        {
            let _ = session.requests.send(Request::GrokSelection(id, highlight));
        }
        result
    }
}

fn binding_icon(binding: &Binding) -> ControlIcon {
    match binding {
        Binding::None | Binding::MainBot | Binding::RecentBot => ControlIcon::None,
        Binding::NewBot => ControlIcon::Add,
        Binding::OpenSelected | Binding::PinnedBot { .. } => ControlIcon::Open,
        Binding::ExitMode => ControlIcon::Exit,
        Binding::Search => ControlIcon::Search,
        Binding::PreviousBot => ControlIcon::Previous,
        Binding::NextBot => ControlIcon::Next,
        Binding::Action(action) => ControlIcon::action(action.kind),
    }
}

fn paint_star(painter: &Painter, center: Pos2, side: f32) {
    let radius = side * key::STAR_RADIUS;
    let [r, g, b] = key::STAR_BACKGROUND;
    painter.circle_filled(center, radius + side * key::UNREAD_OUTLINE, WELL);
    painter.circle_filled(center, radius, Color32::from_rgb(r, g, b));
    let mut star = egui::Mesh::default();
    star.colored_vertex(center, Color32::WHITE);
    for [x, y] in key::star_points(radius * 0.72) {
        star.colored_vertex(center + vec2(x, y), Color32::WHITE);
    }
    for index in 1..=10 {
        star.add_triangle(0, index, index % 10 + 1);
    }
    painter.add(egui::Shape::mesh(star));
}
