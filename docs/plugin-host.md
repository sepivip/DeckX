# Stream Deck plugin compatibility

## Shipped evidence

REA 6.8.0 with Rizin examined the `StreamDeck.exe` extracted from the user's 7.6.1.23033 installer. The same artifact's PE imports identify Qt WebSockets. These strings tie the executable to the published SDK handshake:

| Observation | Virtual address |
| --- | --- |
| `ESDWebSocketConnection` | `0x141641ecc` |
| `registerPlugin` | `0x14164fea8` |
| `-pluginUUID` | `0x141650c30` |
| `connectElgatoStreamDeckSocket` | `0x141650f40` |

REA's archive inventory identified the manifest, Node entry point, and HTML inspector in Elgato's Cat Keys sample. The publisher's Home Assistant 3.8.4 Marketplace release contains an SDK 3 manifest and a browser backend at `plugin.html`. This is why DeckX supports browser backends alongside Node and Windows executables. The binary strings establish shipped identifiers. The public SDK supplies their arguments and message shapes. [Plugin environment](https://docs.elgato.com/streamdeck/sdk/introduction/plugin-environment/) · [WebSocket plugin reference](https://docs.elgato.com/streamdeck/sdk/references/websocket/plugin/) · [Property inspector reference](https://docs.elgato.com/streamdeck/sdk/references/websocket/ui/) · [Home Assistant release](https://github.com/cgiesche/streamdeck-homeassistant/releases/tag/3.8.4)

## DeckX host

- Reads a package's manifest and starts its selected entry point when enabled. Node and Windows processes receive the SDK's port, registration UUID, event, and app/device information. Browser backends receive those fields through the SDK's JavaScript callback.
- Listens on loopback. Plugin and inspector connections register with separate per-session identifiers. Inspector assets resolve inside their plugin directory.
- Sends connection, visibility, key press/release, dial rotation and dial press/release events. Context IDs stay attached to their bindings when moved between controls.
- Stores action and global settings, bridges inspector messages, and handles title, image, state, and confirmation updates. Images are rendered at 64×64 for the AKP03E LCDs. The six LCD keys form the plugin-visible 3×2 display grid. Side buttons can also receive keypad actions.
- Browses the public Marketplace catalog and imports standalone packages from publisher repositories linked by free, unlicensed listings. A download must match the listing's name and publisher and, when GitHub publishes one, the asset's SHA-256 digest. Elgato's official listing handles account acquisition. Touchscreen feedback, plugin-provided profiles, and Elgato hardware or cloud services are outside this compatibility layer.
- Copies every imported package or folder into `%LOCALAPPDATA%\DeckX\PluginHost\Packages`, so later changes to the source do not change what runs. Importing a plugin that is already installed updates it and leaves it disabled until it is approved again. Uninstalling removes the plugin's files, settings, and browser profile.
- Runs each plugin in a Windows job object, so the processes it starts end with it, including when DeckX exits unexpectedly. Manifest paths must stay inside the plugin folder, and SVG artwork can only embed inline SVG images.

## AKP03E canvas and editor colors

The installed AJAZZ Global 3.10.195.0902 binary registers a Qt resource bundle at `0x140001080`. REA disassembled the registration call and identified its resource table arguments. The name table contains `293N3.png` at file offset `0x252e98c`. Its PNG SHA-256 is `b153fde1a7393d90a0e71645a8b1ea7e798fe3150dd0c4258a5e6bc9124f63c7`.

DeckX draws the face using the positions measured from that embedded silhouette. It places six LCDs on the left, three round buttons beneath them, and one large and two small dials on the right. REA also inventoried the bundled AKP03E Music Console preset. Its manifests identify model `20GBA9901`, LCD keys in columns 0–2 and rows 0–1, and knob entries on the right. [AJAZZ product photos](https://ajazzbrand.com/products/ajazz-akp03e-desk-controller-programmable-knob-button-multiple-audio-contro-one-click-opening-of-software-webpage)

The editor uses neutral grays sampled from Elgato's published action assignment capture, including `#2D2D2D` for the canvas, `#292929` for panels, `#222222` for recessed controls, and `#3B3B3B` for raised surfaces. The 840×740 default window uses the reference's 271 px action rail, 50 px category rows, 40 px action rows, and 78 px inspector preview. The device canvas scales with the window and keeps the AJAZZ control positions. [Elgato editor reference](https://help.elgato.com/hc/en-us/articles/16786832942221-Elgato-Stream-Deck-Mobile-2-0-Getting-Started)
