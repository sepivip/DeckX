# Codex Micro interoperability

Work Louder's Input 1.1.0 Windows installer was downloaded from its [publisher release](https://github.com/worklouder/input-releases/releases/tag/v1.1.0) and unpacked for static analysis. Its SHA-256 matches the release asset digest, `09037d324e20cd815124be72162eed84897bb9b762e305914bb3df6c7ca639f2`. REA 6.8.0 analyzed the Input Electron entry point and discovery worker inside `resources/app.asar` (SHA-256 `1ee00f9a2c0b955c77bb669186aed2e2e76d90bd78be3c2bff4383263fdde633`).

The installed ChatGPT desktop bundle identifies itself as `openai-codex-electron` version `26.1103.60933`. REA analyzed the selected Codex Micro service and `@worklouder/device-kit-oai` 0.2.1 RPC client from its ASAR. The service receives hardware inputs and sends lighting based on chat state. Input uses the separate base Work Louder SDK to discover hardware and configure device settings.

| Interface | Observed behavior |
| --- | --- |
| Work Louder discovery | USB vendor `303a`, vendor HID usage page `ff00`, Codex Micro product `8360`, Creator Micro 2 products `8297` and `8298` |
| Device RPC | 64-byte reports with a channel and payload length. Key and joystick notifications arrive separately from status and lighting commands. |
| Codex desktop | Windows registers `codex:`. The app handles launch and thread links. The new-thread route accepts a mode and a prompt to put in the composer. |
| Desktop chat state | The renderer maps six slots to idle, working, approval, input, unread, error, or off. The default slot source is recent chats. The main process uses a 350 ms double-tap window. REA evidence for the renderer mapping is `ev_5414b5b87074c98a0356d797acd917fb99949a56b4889da11c71e440ffc02939`. |
| DeckX Codex Mode | The local session index supplies the six recent chat titles and IDs. DeckX subscribes as its own client to those IDs on the local Codex IPC router, projecting runtime status, pending requests, and unread state from snapshots and patches. The status overlay occupies the AJAZZ LCD keys until Codex Mode exits. |
| Settings page | The Codex desktop settings chunk `474658.278bfaa408.js` puts connection settings first, the centered hardware preview under Layout, and device behavior under Options. DeckX adapts that structure and the Micro's status palette to the AKP03E face measured from AJAZZ's `293N3.png`. Both DeckX views use the same control coordinates. REA evidence is `ev_4d3870f2ccd8ad27f76e5362f9e0ebb9f3c336befdc672446294b47569bad035`. |

Work Louder's [Codex Micro setup](https://www.worklouder.cc/setup/codex-micro) describes the six agent keys, dial, joystick, lighting states, and configurable layers. Elgato's [ChatGPT & Codex plugin](https://marketplace.elgato.com/product/chatgpt-codex-fb6a7052-2b1a-4500-ba2a-13996edf0b47) is a separate product. The built-in DeckX actions use the desktop app's link handler and require the desktop app to be installed.

Codex Mode uses the desktop link to open a chat. A single tap selects the slot in DeckX. The desktop app must publish a chat snapshot before DeckX can show its live status. Status can be unavailable for inactive chats, remote chats, or a desktop build that changes the local protocol. Native background focus, the Micro's approval and voice commands, and its joystick and firmware controls are outside the current implementation.

Classic uses the RGB values in the installed Codex build's `.vite/build/early-bootstrap.js` for the whole key face. REA evidence is `ev_6ef75551c9f067da5cbaec4c74cc21618239108ca25974226dd8f797877ddb2a`.

| Codex Micro state | Classic RGB |
| --- | --- |
| Off or waiting for status | `#000000` |
| Idle | `#FFFFFF` |
| Working | `#304FFE` |
| Awaiting approval or response | `#FF6D00` |
| Unread | `#00FF4C` |
| Error | `#FF0033` |

Color titles use a separate contrast-adjusted palette. The AKP03E receives JPEG LCD images, so its observed color also depends on JPEG decoding, brightness, and the LCD itself.
