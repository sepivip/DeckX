#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod agent_layout;
mod asar;
mod claude;
mod codex;
mod deck_model;
mod device;
mod easter_egg;
mod foreground;
mod glyphs;
mod grok;
mod instance;
mod marketplace;
mod mcp;
mod plugins;
mod profile;
mod runtime;
mod startup;
mod theme;
mod tray;
mod ui;
mod usage;
mod workspace;

use eframe::egui::{ViewportBuilder, vec2};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> eframe::Result {
    let command = std::env::args_os().nth(1);
    if command
        .as_deref()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("DeckX {APP_VERSION}");
        return Ok(());
    }
    if command.as_deref().is_some_and(|arg| arg == "--mcp") {
        if let Err(error) = mcp::stdio() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if command.as_deref().is_some_and(|arg| arg == "--probe") {
        match device::Deck::open() {
            Ok(_) => println!("Opened AJAZZ AKP03E vendor HID interface (0300:3002)."),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let from_startup = std::env::args_os().any(|arg| arg == startup::ARG);
    if !instance::claim_within(std::time::Duration::from_secs(2)) {
        if !from_startup {
            instance::focus_running();
        }
        return Ok(());
    }

    let startup = startup::State::load(!from_startup);
    let connect = from_startup || std::env::args_os().any(|arg| arg == "--connect");
    use runtime::ModeKind;
    let view = if std::env::args_os().any(|arg| arg == "--codex-mode") {
        ModeKind::Codex
    } else if std::env::args_os().any(|arg| arg == "--grok-mode") {
        ModeKind::Grok
    } else if std::env::args_os().any(|arg| arg == "--claude-mode") {
        ModeKind::Claude
    } else {
        ModeKind::Workspace
    };
    eframe::run_native(
        "DeckX",
        eframe::NativeOptions {
            viewport: ViewportBuilder::default()
                .with_inner_size(vec2(840.0, 740.0))
                .with_min_inner_size(vec2(700.0, 580.0))
                .with_icon(theme::icon()),
            ..Default::default()
        },
        Box::new(move |context| Ok(Box::new(ui::DeckX::new(context, connect, view, startup)))),
    )
}
