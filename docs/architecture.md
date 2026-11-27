# Layout and device boundaries

## UI design

`src/theme/tokens.rs` defines the DeckX palette, typography, spacing, and control sizes. `src/theme.rs` applies those tokens to egui and provides the shared buttons, icons, cards, and layout helpers. `src/theme/forms.rs` provides the fields, menus, rows, and dialogs used by Workspace, Settings, Plugins (including Marketplace), and the agent modes. Add reusable UI here and let the screens supply their labels, data, and actions.

## Deck layout

Workspace, Codex, Grok Bot, and Claude all render the device face through `ui/deck_face.rs`. `deck_model.rs` describes the AKP03E face and its LCD, side button, and dial positions. The face renderer scales those positions and exposes the same controls and hit bounds to each view.

`ui/mode_page.rs` controls how an agent layout is displayed and edited. `ui/mode_editor.rs` owns the draft, dial tabs, action menu, defaults, save, and cancel flow. Each mode supplies its bindings, available items, and LCD contents. The Workspace uses the same face with its own drag and drop and inspector.

Agent-key presets live in `agent_layout.rs`. Layouts saved before presets default to the standard layout. A preset can reserve an LCD position while retaining its saved binding. The resolved layout skips that position when selecting or opening an agent. The agent-mode page and HID runtime both use the reserved position and the same usage tile renderer.

The usage monitor runs off the UI and HID threads while a usage preset is active. It reads the signed-in Codex account through the Codex App Server, Grok Bot’s weekly cache for the current roster scope, or Claude Desktop’s most recent usage sample. The Grok reader accepts personal and team usage when the roster and snapshot scopes agree. Claude readings stay available as last-seen values within Desktop’s 30-day history window. The detail view shows the source and age. Grok Bot refreshes its cache from Usage & Billing, and Claude Desktop can refresh its cache when its Windows tray menu opens. [Codex App Server](https://developers.openai.com/codex/app-server) · [Grok Bot settings](https://docs.x.ai/grok-bot/settings-and-notifications) · [Claude usage limits](https://support.claude.com/en/articles/11647753-how-do-usage-and-length-limits-work)

## Device runtime

`runtime.rs` owns the deck on the `DeckX HID` thread. A `Runtime` holds the session state and the active workspace or agent mode. HID transport errors do not end the session. The runtime reports `Reconnecting`, polls for the deck every second, and redraws the current page or mode when it returns. UI requests sent meanwhile run after the reconnect.

Bound actions and multi actions run on two long-lived worker threads with a COM apartment, so a slow app launch or shell call cannot block key reads or the keep-alive. Page changes, saves and mode changes cancel a running multi action before its next step. A key or dial release goes to the plugin that received the press, even after a page change.

## Adding a deck

When adding another deck, keep its dimensions and control positions in a model descriptor and its wire protocol in a device backend. Reuse the face renderer and assignment editor. Before supporting a model with different control counts, make persisted layouts model-aware: existing Workspace and agent settings use fixed AKP03E counts and must continue to load as AKP03E layouts.

On Windows, `cargo test --locked` checks the editor with synthetic input and temporary settings. It does not need a deck or a companion app. The ignored offscreen rendering test needs EGL and the installed Grok Bot bundle; set `DECKX_VISUAL_DIR` to save its PNGs. Physical HID behavior is tested separately.
