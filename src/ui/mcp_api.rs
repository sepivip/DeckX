use anyhow::{Context as _, Result, bail, ensure};
use eframe::egui::{Context, UserAttentionType, ViewportCommand};
use serde_json::{Value, json};

use crate::{
    actions::{Action, ActionKind},
    codex,
    mcp::{
        self, Brightness, EditCodex, EditPlugin, EditWorkspace, ImportWorkspace, PluginScope,
        PluginSettings, WorkspaceChange, decode,
    },
    plugins::Instance,
    profile::{Control, DISPLAY_KEYS},
    runtime::{Active, Connection},
    workspace::Workspace,
};

use super::{DeckX, View};

impl DeckX {
    pub(super) fn poll_mcp(&mut self, ctx: &Context) {
        for _ in 0..8 {
            let Some(endpoint) = &self.mcp else { return };
            let pending = match endpoint.incoming.try_recv() {
                Ok(pending) => pending,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.mcp = None;
                    self.mcp_settings.server_error =
                        Some("The local server stopped unexpectedly.".into());
                    return;
                }
            };
            let client_request = pending.name != mcp::PROBE;
            if pending.answer(|name, arguments| self.mcp_tool(ctx, name, arguments))
                && client_request
            {
                self.mcp_settings.last_request = Some(std::time::Instant::now());
            }
        }
        ctx.request_repaint();
    }

    fn mcp_tool(&mut self, ctx: &Context, name: &str, args: Value) -> Result<Value> {
        match name {
            mcp::PROBE => Ok(json!({})),
            "deckx_state" => {
                let _: mcp::Empty = decode(args)?;
                Ok(self.mcp_state())
            }
            "deckx_edit_workspace" => self.mcp_edit_workspace(decode(args)?),
            "deckx_import_workspace" => {
                let ImportWorkspace { path } = decode(args)?;
                self.mcp_workspace_ready()?;
                mcp::absolute(&path)?;
                ensure!(path.is_file(), "Workspace file is missing");
                let next = Workspace::load(&path)?;
                self.save_workspace(next)?;
                Ok(self.mcp_state())
            }
            "deckx_set_brightness" => {
                let Brightness { brightness } = decode(args)?;
                if brightness != self.saved.brightness
                    || (self.live_brightness.is_some() && self.live_brightness != Some(brightness))
                {
                    self.save_brightness(brightness)?;
                    self.report("Brightness saved", false);
                }
                Ok(
                    json!({"saved_brightness": self.saved.brightness, "applied_brightness":self.live_brightness}),
                )
            }
            "deckx_connect" => {
                let mcp::Connection { connected } = decode(args)?;
                match (
                    connected,
                    self.session.as_ref().map(|session| session.connection),
                ) {
                    (true, None) => self.connect(ctx)?,
                    (true, Some(Connection::Closing)) => {
                        bail!("Wait for the deck to disconnect, then connect again")
                    }
                    (
                        false,
                        Some(Connection::Opening | Connection::Open | Connection::Reconnecting),
                    ) => self.toggle_connection(ctx),
                    _ => {}
                }
                Ok(json!({"connection":self.mcp_connection()}))
            }
            "deckx_codex_mode" => {
                let _: mcp::Empty = decode(args)?;
                Ok(self.mcp_codex_state())
            }
            "deckx_edit_codex_mode" => self.mcp_edit_codex(decode(args)?),
            "deckx_plugins" => {
                let _: mcp::Empty = decode(args)?;
                self.mcp_plugins()
            }
            "deckx_edit_plugin" => self.mcp_edit_plugin(ctx, decode(args)?),
            "deckx_plugin_settings" => self.mcp_plugin_settings(decode(args)?),
            _ => bail!("Unknown DeckX tool"),
        }
    }

    fn mcp_connection(&self) -> &'static str {
        match self.session.as_ref().map(|session| session.connection) {
            None => "disconnected",
            Some(Connection::Opening) => "connecting",
            Some(Connection::Open) => "connected",
            Some(Connection::Reconnecting) => "reconnecting",
            Some(Connection::Closing) => "disconnecting",
        }
    }

    fn mcp_state(&self) -> Value {
        let last_input = self
            .recent
            .map(|(_, control, _)| json!({"control":control.label(), "count":self.input_count}));
        let claude_selected = match &self.mode.active {
            Active::Claude(mode) => mode.selected.map(|index| index + 1),
            _ => None,
        };
        json!({"model":"AJAZZ AKP03E", "version":crate::APP_VERSION,
            "connection":self.mcp_connection(), "workspace":self.saved,
            "editor_page":self.scene,"active_page":self.live,"applied_brightness":self.live_brightness,
            "codex_mode":self.view() == View::Codex, "grok_mode":self.view() == View::Grok,
            "claude_mode":self.view() == View::Claude, "claude_selected_key":claude_selected,
            "unsaved_editor_changes":self.draft != self.saved, "last_input":last_input,
            "easter_egg_visible":self.easter_egg_visible,
            "in_tray":self.tray.as_ref().is_some_and(crate::tray::Tray::is_hidden),
            "message":self.message,"error":self.error})
    }

    fn mcp_workspace_ready(&self) -> Result<()> {
        ensure!(
            self.draft == self.saved,
            "Save or revert the unsaved DeckX editor changes first"
        );
        Ok(())
    }

    fn mcp_edit_workspace(&mut self, edit: EditWorkspace) -> Result<Value> {
        self.mcp_workspace_ready()?;
        ensure!(
            (1..=128).contains(&edit.changes.len()),
            "Supply 1–128 workspace changes"
        );
        let path = self
            .path
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?;
        ensure!(
            Workspace::load(path)? == self.saved,
            "The saved workspace changed on disk. Reopen DeckX before editing"
        );
        let mut next = self.saved.clone();
        next.home = self.scene;
        let mut created = Vec::new();
        for change in edit.changes {
            match change {
                WorkspaceChange::AddProfile { name } => {
                    created.push(next.add_profile(name_value(name)?)?);
                }
                WorkspaceChange::RenameProfile { profile, name } => {
                    next.profile_mut(profile)
                        .context("Profile is missing")?
                        .name = name_value(name)?;
                }
                WorkspaceChange::RemoveProfile { profile } => {
                    ensure!(next.profile(profile).is_some(), "Profile is missing");
                    next.remove_profile(profile)?;
                }
                WorkspaceChange::LinkApp { profile, app } => {
                    if let Some(path) = &app {
                        mcp::absolute(path)?;
                        ensure!(path.is_file(), "Choose an existing Windows application");
                    }
                    next.profile_mut(profile).context("Profile is missing")?.app = app;
                }
                WorkspaceChange::AddPage {
                    profile,
                    name,
                    folder,
                } => {
                    created.push(next.add_page(profile, name_value(name)?, folder)?);
                }
                WorkspaceChange::RenamePage { scene, name } => {
                    next.page_mut(scene).context("Page is missing")?.name = name_value(name)?;
                }
                WorkspaceChange::RemovePage { scene } => {
                    next.remove_page(scene)?;
                }
                WorkspaceChange::SelectPage { scene } => {
                    ensure!(next.page(scene).is_some(), "Page is missing");
                    next.home = scene;
                }
                WorkspaceChange::SetDisplay {
                    brightness,
                    rotation,
                } => {
                    next.brightness = brightness.unwrap_or(next.brightness);
                    next.rotation = rotation.unwrap_or(next.rotation);
                }
                WorkspaceChange::SetAction {
                    scene,
                    target,
                    action,
                } => {
                    let control = target.control()?;
                    let layout = &mut next.page_mut(scene).context("Page is missing")?.layout;
                    self.mcp_assign(layout, control, action)?;
                }
                WorkspaceChange::SetLcdAppearance {
                    scene,
                    index,
                    label,
                    image,
                    color,
                    clear_image,
                    clear_color,
                } => {
                    ensure!(
                        (1..=DISPLAY_KEYS).contains(&index),
                        "LCD key index must be between 1 and 6"
                    );
                    ensure!(
                        !clear_image || image.is_none(),
                        "Choose either image or clear_image"
                    );
                    ensure!(
                        !clear_color || color.is_none(),
                        "Choose either color or clear_color"
                    );
                    let button = &mut next
                        .page_mut(scene)
                        .context("Page is missing")?
                        .layout
                        .buttons[index - 1];
                    if let Some(label) = label {
                        crate::profile::check_label(&label)?;
                        button.label = label;
                    }
                    if let Some(path) = &image {
                        mcp::absolute(path)?;
                    }
                    if clear_image || image.is_some() {
                        button.image = image;
                    }
                    if clear_color || color.is_some() {
                        button.color = color;
                    }
                }
            }
        }
        if next != self.saved {
            self.save_workspace(next)?;
        }
        Ok(json!({"created_pages":created,"state":self.mcp_state()}))
    }

    fn mcp_assign(
        &self,
        layout: &mut crate::profile::Layout,
        control: Control,
        mut action: Action,
    ) -> Result<()> {
        if action.kind == ActionKind::Plugin {
            let item = self
                .plugin_item(action.value.trim())
                .context("Install the plugin action before binding it")?;
            ensure!(
                super::plugin_fits(&item, control),
                "The plugin action does not support this control"
            );
            let current = layout.action(control);
            // Rebinding the same plugin action keeps its context and settings.
            action = if current.kind == ActionKind::Plugin && current.value == item.uuid {
                current.clone()
            } else {
                Action::plugin(item.uuid)
            };
        }
        layout.set_action(control, action);
        Ok(())
    }

    fn mcp_codex_state(&self) -> Value {
        let chats = codex::recent_chats();
        let layout = self.codex_agent.saved.resolve(&chats);
        let slots = layout.agents.iter().enumerate().map(|(index, chat)| {
            chat.as_ref().map(|chat| json!({"key":index+1,"id":chat.id.to_string(),"title":chat.title,"status":self.codex.status(chat.id).label()}))
        }).collect::<Vec<_>>();
        let chats = chats
            .iter()
            .map(|chat| json!({"id":chat.id.to_string(),"title":chat.thread_name}))
            .collect::<Vec<_>>();
        let selected_key = match &self.mode.active {
            Active::Codex(mode) => mode.selected.map(|index| index + 1),
            _ => None,
        };
        json!({"enabled":self.view() == View::Codex,"layout":self.codex_agent.saved,
            "selected_key":selected_key,
            "applied_key_style":self.live_codex.map(|(style,_)| style),
            "last_lcd_writes":self.live_codex.map(|(_,count)| count),"slots":slots,"recent_chats":chats,
            "layout_error":self.codex_agent.load_error,"status_error":self.codex.error()})
    }

    fn mcp_edit_codex(&mut self, edit: EditCodex) -> Result<Value> {
        let mut next = if edit.reset_assignments {
            codex::Layout::default()
        } else {
            self.codex_agent.saved.clone()
        };
        next.key_style = edit.key_style.unwrap_or(self.codex_agent.saved.key_style);
        for assignment in edit.bindings {
            *next.binding_mut(assignment.target.control()?) = assignment.binding;
        }
        if next != self.codex_agent.saved || edit.reset_assignments {
            ensure!(
                self.codex_agent.editor.is_none(),
                "Close the open Codex Mode assignment editor first"
            );
            self.save_codex_layout(next)?;
        }
        if let Some(enabled) = edit.enabled {
            let next = if enabled {
                super::View::Codex
            } else if self.view() == View::Codex {
                super::View::Workspace
            } else {
                self.view()
            };
            self.set_view(next);
        }
        Ok(self.mcp_codex_state())
    }

    fn mcp_plugins(&self) -> Result<Value> {
        let host = self
            .plugins
            .as_ref()
            .context("Plugin host is unavailable")?;
        let plugins = host.installed().into_iter().map(|plugin| {
            json!({"id":plugin.id,"name":plugin.name,"detail":plugin.detail,"enabled":plugin.enabled,
                "status":plugin.status,"source":plugin.source})
        }).collect::<Vec<_>>();
        let actions = host.catalog().into_iter().map(|action| {
            json!({"uuid":action.uuid,"plugin_id":host.owner(&action.uuid),"name":action.name,
                "category":action.category,"description":action.tooltip,"lcd":action.key,"dial":action.dial,"has_inspector":action.inspector})
        }).collect::<Vec<_>>();
        Ok(json!({"plugins":plugins,"actions":actions,"node":host.node(),"error":host.error()}))
    }

    fn mcp_edit_plugin(&mut self, ctx: &Context, edit: EditPlugin) -> Result<Value> {
        let host = self
            .plugins
            .as_mut()
            .context("Plugin host is unavailable")?;
        match edit {
            EditPlugin::Import { path } => {
                mcp::absolute(&path)?;
                host.import(&path)?;
            }
            EditPlugin::Enable {
                plugin_id,
                enabled: true,
            } => {
                let running = host
                    .installed()
                    .into_iter()
                    .find(|plugin| plugin.id == plugin_id)
                    .map(|plugin| plugin.enabled)
                    .context("Plugin is missing")?;
                if !running {
                    // Enabling runs the plugin's code. Only the user can approve that, in DeckX.
                    return self.request_plugin_approval(ctx, plugin_id);
                }
                host.set_enabled(&plugin_id, true)?;
            }
            EditPlugin::Enable {
                plugin_id,
                enabled: false,
            } => host.set_enabled(&plugin_id, false)?,
        }
        self.mcp_plugins()
    }

    fn request_plugin_approval(&mut self, ctx: &Context, plugin_id: String) -> Result<Value> {
        self.plugin_enable = Some(plugin_id);
        self.plugin_window = true;
        self.marketplace_tab = false;
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
            UserAttentionType::Informational,
        ));
        self.report(
            "Codex asked to enable a plugin. Review it in Plugins",
            false,
        );
        let mut state = self.mcp_plugins()?;
        state["approval"] = json!(
            "Waiting for the user. DeckX asked them to approve this plugin, and it starts only if they choose Enable plugin. Call deckx_plugins later to check its status."
        );
        Ok(state)
    }

    fn mcp_plugin_settings(&self, args: PluginSettings) -> Result<Value> {
        let host = self
            .plugins
            .as_ref()
            .context("Plugin host is unavailable")?;
        let settings = args.settings.map(Value::Object);
        let value = match args.scope {
            PluginScope::Global { plugin_id } => host.configure(&plugin_id, None, settings)?,
            PluginScope::Binding { scene, target } => {
                let control = target.control()?;
                let action = self
                    .saved
                    .page(scene)
                    .context("Page is missing")?
                    .layout
                    .action(control);
                ensure!(
                    action.kind == ActionKind::Plugin,
                    "This control does not have a plugin binding"
                );
                let plugin = host.owner(&action.value).context("Plugin is missing")?;
                let context = action
                    .plugin_context
                    .clone()
                    .context("Plugin binding is missing its context")?;
                host.configure(
                    plugin,
                    Some(&Instance {
                        context,
                        action: action.value.clone(),
                        control,
                    }),
                    settings,
                )?
            }
        };
        Ok(json!({"settings":value}))
    }
}

fn name_value(name: String) -> Result<String> {
    let name = name.trim();
    let limit = crate::workspace::NAME_CHARS;
    ensure!(
        (1..=limit).contains(&name.chars().count()),
        "Enter a name of 1–{limit} characters"
    );
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{profile::Direction, workspace::Scene};

    const PAGE: Scene = Scene {
        profile: 1,
        page: 2,
    };

    fn app() -> (tempfile::TempDir, Context, DeckX) {
        let folder = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let path = folder.path().join("profile.json");
        let app = DeckX::load(&ctx, Ok(path), codex::Activity::fixture());
        (folder, ctx, app)
    }

    fn edit(app: &mut DeckX, ctx: &Context, changes: Value) -> Result<Value> {
        app.mcp_tool(ctx, "deckx_edit_workspace", json!({ "changes": changes }))
    }

    fn set_action(target: Value, kind: &str) -> Value {
        json!({"op":"set_action","scene":PAGE,"target":target,"action":{"kind":kind}})
    }

    #[test]
    fn a_failing_change_leaves_the_whole_batch_unsaved() {
        let (folder, ctx, mut app) = app();
        let result = edit(
            &mut app,
            &ctx,
            json!([
                {"op":"add_profile","name":"Work"},
                {"op":"rename_profile","profile":999,"name":"Missing"},
            ]),
        );
        assert!(result.is_err());
        assert!(!folder.path().join("profile.json").exists());
        assert_eq!(app.saved, Workspace::default());
    }

    #[test]
    fn controls_are_numbered_from_one() {
        let (folder, ctx, mut app) = app();
        edit(
            &mut app,
            &ctx,
            json!([
                set_action(json!({"kind":"lcd","index":1}), "Mute"),
                set_action(json!({"kind":"side","index":3}), "NextTrack"),
                set_action(
                    json!({"kind":"dial","index":3,"gesture":"press"}),
                    "PlayPause"
                ),
            ]),
        )
        .unwrap();
        let saved = Workspace::load(&folder.path().join("profile.json")).unwrap();
        let layout = &saved.page(PAGE).unwrap().layout;
        assert_eq!(layout.action(Control::Button(0)).kind, ActionKind::Mute);
        assert_eq!(
            layout.action(Control::Button(DISPLAY_KEYS + 2)).kind,
            ActionKind::NextTrack
        );
        assert_eq!(
            layout.action(Control::Dial(2, Direction::Press)).kind,
            ActionKind::PlayPause
        );
        for target in [
            json!({"kind":"lcd","index":0}),
            json!({"kind":"lcd","index":7}),
            json!({"kind":"side","index":4}),
        ] {
            assert!(edit(&mut app, &ctx, json!([set_action(target, "Mute")])).is_err());
        }
    }

    #[test]
    fn labels_must_fit_the_key() {
        let (_folder, ctx, mut app) = app();
        let label =
            |label: &str| json!([{"op":"set_lcd_appearance","scene":PAGE,"index":1,"label":label}]);
        assert!(edit(&mut app, &ctx, label("Too long for a key")).is_err());
        edit(&mut app, &ctx, label("Focus")).unwrap();
        assert_eq!(
            app.saved.page(PAGE).unwrap().layout.buttons[0].label,
            "Focus"
        );
    }

    #[test]
    fn names_follow_the_editor_limit() {
        let (_folder, ctx, mut app) = app();
        let add = |name: String| json!([{"op":"add_profile","name":name}]);
        let limit = crate::workspace::NAME_CHARS;
        assert!(edit(&mut app, &ctx, add("x".repeat(limit + 1))).is_err());
        assert!(edit(&mut app, &ctx, add("x".repeat(limit))).is_ok());
    }

    #[test]
    fn edits_wait_for_unsaved_editor_changes() {
        let (_folder, ctx, mut app) = app();
        app.draft.brightness = 1;
        let result = edit(&mut app, &ctx, json!([{"op":"add_profile","name":"Work"}]));
        assert!(result.is_err());
    }
}
