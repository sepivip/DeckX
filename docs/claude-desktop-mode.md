# Claude Desktop mode

Claude mode gives saved Claude chats fixed positions on the deck. Each pinned key sends its chat link to Claude Desktop in one press. Claude mode selects the first pin on entry so the full saved title appears below the deck preview. Hover over any pinned key in the preview to see its title.

## Layout and controls

Put **Claude** after **Grok Bot** in the toolbar. Use the existing mode page with Brightness, Layout, Reset layout, the deck preview, and Options. Reuse the same assignment editor and design tokens. The agent keys use neutral title tiles with the standard selection outline and ellipsis for long titles. The summary under the deck shows the selected chat and an **Open chat** button.

| Control | Default |
| --- | --- |
| Six LCD keys | Unassigned until the user pins a chat or saves an action |
| Main dial | Turn to select a pinned chat, skipping empty and action-only keys. Press to open it. |
| Side buttons, left to right | New chat, Open selected chat, Exit mode |
| Two smaller dials | Unassigned. Use the shared editor to assign each gesture. |

Click an LCD key to open its assignment editor. Choose **Pin a Claude chat**, enter the chat link or UUID, give the key an **LCD title**, and save. The private chat address can be copied from the browser while signed into Claude. The saved key opens the chat under the account signed into Claude Desktop. **Use default** clears an LCD assignment. Right-click offers the same edit and open commands as the other modes. [Claude Desktop deep links](https://support.claude.com/en/articles/14729294-open-claude-desktop-with-a-link)

## Evidence from the installed app

| Check | Result |
| --- | --- |
| Windows registration | Claude Desktop **6.18355.0.0** declares the `claude` protocol. Windows returned `Available` for a read-only URI support query scoped to the installed Claude package. |
| Navigation | Anthropic supports `claude://claude.ai/new` and `claude://claude.ai/chat/{conversation-id}` on Windows. The shipped main bundle has a second-instance argument handler and a New Chat action that uses the protocol. |
| Desktop Quick Entry | The packaged `.vite/build/mainView.js` preload sends `QuickEntry.setRecentChats` to the main process. The installed bundle stores `{chatId, chatName}` rows and the current ID in memory. Both readers of the list feed the macOS Swift Quick Entry overlay. |
| Windows Jump List | The packaged builder adds New Chat, Claude Code shortcuts, and Code or Cowork sessions waiting for input. The builder never reads the Claude Chat recent list. |
| Windows accessibility | The inspected desktop tree exposed panes, documents, and window controls. The observed fields had no chat rows or chat paths. The window was minimized during the activation check, so the expanded sidebar still needs inspection. |
| Claude on the web | The visible **Recents** sidebar exposes each row's display title with an exact `/chat/{uuid}` link, in the displayed order. This yielded six distinct named chats after skipping an untitled row. |

Windows accepted a dispatch of the saved chat URI while Claude Desktop was minimized. The window remained minimized at the next observation. Confirm the destination, foreground restoration, and cold launch separately. A supported Windows chat catalog and live Chat activity feed remain unverified. [Claude Desktop deep links](https://support.claude.com/en/articles/14729294-open-claude-desktop-with-a-link)

REA inspected the Electron archive version `6.18355.0`, SHA-256 `176c011b0413d562b03d37884b0d76bb0afad8f818397089efd72adbf8e7d382`. The retained analysis is `ev_67c283673a7ada9746e790214d1ce3135e303bd928f6e244b21292ceaa277178`. It covers the selected launcher and preloads. REA recorded six digest contradictions in unpacked companion files. The main-process observations come from a focused static read of packed JavaScript.

## Implementation order

1. **Implement the launch action.** Verify manual URI navigation with a test chat. Accept a UUID or an HTTPS or Claude URL with the exact `claude.ai/chat/{uuid}` host and path. Construct a fresh deep link from the parsed ID, dropping any query or fragment. Use one prepared action for the preview’s **Open chat** and the physical controls. Queue the launch off the HID thread. Show an actionable message if Windows cannot dispatch it. Treat Windows’ return value as a dispatch result and verify the selected chat in the hardware test. Claude handles sign-in and access to the chat. The user sends any prompt from Claude.
2. **Add the Claude page and storage.** Adapt `ModePage` and `Assignments` to the Claude layout. Persist each pin’s UUID and display title in `claude-layout.json` using the shared validated save path. Keep the last saved layout when a draft is invalid. Extract the status-independent part of the Codex title renderer and use DeckX’s bundled font for Claude titles. [Shared mode page](../src/ui/mode_page.rs) · [Assignment editor](../src/ui/mode_editor.rs) · [Layout store](../src/agent_layout.rs)
3. **Share the mode transition.** Use the runtime’s active-mode enum and a common mode-change request for entry and exit. Preserve each mode’s existing key behavior while keeping device I/O and geometry in the shared backend. Refresh Claude tiles on assignment or selection changes. Preserve pins across restarts, account changes, and periods when Claude is unavailable. [Runtime](../src/runtime.rs) · [Deck geometry](../src/deck_model.rs)
4. **Verify the launch path and hardware.** Check open, minimized, and closed Claude with real test chats. Verify one key press dispatches one link and opens the exact pinned chat. Check new-chat, dial, exit, reconnect, invalid links, inaccessible chats, missing Claude, and signed-out Claude. Use two chats with the same title and verify their pins survive restarting and updating DeckX. Check Workspace, Codex, and Grok Bot after the common transition changes. Render the populated and empty layout, assignment and error states, and dial editor at 840 × 740 and 700 × 580 at 100%, 125%, and 150%.

## Follow-up capabilities

Support an explicit refresh from the visible Claude web sidebar. A Chromium extension using `activeTab` can read the displayed titles and chat links after the user invokes it on Claude. Pass only those pairs to DeckX through a local import with a preview. Keep pins in their slots, exclude their UUIDs from the imported list, and preserve dial selection by UUID across reordering. If the sidebar is unavailable, keep the last saved layout and offer manual pinning. [Chrome activeTab documentation](https://developer.chrome.com/docs/extensions/develop/concepts/activeTab)

Anthropic's data export includes chat history and arrives by email. It can provide an offline snapshot when deliberately supplied by the user. A sidebar refresh uses a much smaller set of data and can be repeated on demand. [Export your Claude data](https://support.claude.com/en/articles/9450526-export-your-claude-data)

Gate activity colors on an observed status feed and its update behavior. Claude Desktop also documents new-session links for Code and Cowork, which can become explicit actions after Windows lifecycle testing. [Claude Desktop deep links](https://support.claude.com/en/articles/14729294-open-claude-desktop-with-a-link)
