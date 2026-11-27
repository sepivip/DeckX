# DeckX MCP tools

Open **Settings → MCP** in DeckX and select **Add to Codex**. Restart Codex and ask “Show my DeckX setup.” **Test connection** checks whether the running DeckX app responds locally. **Last client request** updates when a client sends an MCP request. DeckX remembers the local server’s start or stop setting.

The server command `deckx.exe --mcp` forwards requests to the running DeckX app. Keep DeckX open while using the tools. For manual registration, use the setup command in the MCP tab or the [README](../README.md#configure-deckx-through-codex).

| Tool | Use |
| --- | --- |
| `deckx_state` | Read the saved workspace, current page, connection, and applied brightness. |
| `deckx_edit_workspace` | Batch changes to profiles, pages, folders, app associations, display settings, actions, and LCD appearance. |
| `deckx_import_workspace` | Load a DeckX workspace JSON file from an absolute path and save it as the active workspace. |
| `deckx_set_brightness`, `deckx_connect` | Set brightness or connect and disconnect the AJAZZ. |
| `deckx_codex_mode`, `deckx_edit_codex_mode` | Inspect or change the mode, key style, and key and dial assignments. |
| `deckx_plugins`, `deckx_edit_plugin` | List, import, enable, or disable plugins. Enabling a plugin asks the user to approve it in DeckX, and it starts only after they do. Choose the Node.js runtime in DeckX itself. |
| `deckx_plugin_settings` | Read or replace global plugin settings or settings for a saved binding. Omit `settings` to read. |
| `deckx_search_marketplace`, `deckx_install_marketplace` | Browse the Elgato catalog and install supported public publisher packages. |

Before editing, read the workspace and use the IDs it returns. For example, if profile `1` has page `2`, this batch assigns mute to dial 1's press and sets the first LCD key's label and background color.

```json
{
  "changes": [
    {"op":"set_action","scene":{"profile":1,"page":2},"target":{"kind":"dial","index":1,"gesture":"press"},"action":{"kind":"Mute"}},
    {"op":"set_lcd_appearance","scene":{"profile":1,"page":2},"index":1,"label":"Focus","color":[48,79,254]}
  ]
}
```

LCD keys run from 1 to 6. Side buttons and dials run from 1 to 3. A dial gesture is `left`, `press`, or `right`. To bind a plugin, use action kind `Plugin` and put the action UUID from `deckx_plugins` in `value`. A plugin encoder binds the whole dial. Configure the binding after saving it.

To switch on Codex Mode and use solid status colors, call `deckx_edit_codex_mode` with `{"enabled":true,"key_style":"classic"}`. A binding can use a built-in mode command such as `"OpenSelected"`, or a specific chat with `{"Action":{"kind":"CodexOpenChat","value":"<chat-id>"}}`.

Save or revert any open workspace edits before making MCP workspace changes. Device writes run asynchronously. Read back `active_page`, `applied_brightness`, or `applied_key_style` to confirm that the connected deck received the change.
