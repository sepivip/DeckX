# Elgato Stream Deck 7.6.1

Analyzed the downloaded Windows installer `Stream_Deck_7.6.1.23033.msi` offline. The MSI was extracted with lessmsi. The package and extracted executable were inspected as files.

| Artifact | SHA-256 |
| --- | --- |
| Installer | `a42157fa5a44f1bc0426e7aa0fa25856206bfc811f5162d3f283704172b7b3cf` |
| `StreamDeck.exe` | `d9487481d42e5c8d13254c813d5c80158fb58d77c37e8c89036549f3aae38618` |
| Mini default profile archive | `4ca31e3895908f7f28ded6a9c8db8a0ce1135f32da26677eecc06a64b5568016` |

| Evidence | Observation | DeckX implementation |
| --- | --- | --- |
| REA PE imports | Native x64 executable imports Qt 6 Widgets, Quick, WebEngine, WebSockets, and HID | Native Rust editor around the existing AKP03E transport |
| REA Rizin strings | `PropertyInspectorBase` plus Multi Action and Dial inspector helpers. Action-list strings include move, copy, and paste events. | Searchable action palette, control canvas, property inspector, and drag or context menu editing |
| REA archive inventory | The Mini default profile is a ZIP with nested profile manifests and image files | A profile can hold multiple layouts, with a preview of each configured key |
| Shipped plugin manifests | System hotkey, text, file, app, website, and media actions. Page navigation, child and parent folders, profile switching, and Multi Action with Delay. | Built-in actions for the corresponding local workflows |

Elgato's user guides describe [drag and drop assignment](https://help.elgato.com/hc/en-us/articles/16786832942221-Elgato-Stream-Deck-Mobile-2-0-Getting-Started), [pages](https://help.elgato.com/hc/en-us/articles/4410312027277-Elgato-Stream-Deck-Pages), [folders](https://help.elgato.com/hc/en-us/articles/360027957912-Elgato-Stream-Deck-Using-Folders), [app associations](https://help.elgato.com/hc/en-us/articles/360053419071-Elgato-Stream-Deck-Smart-Profiles), and [ordered multi actions](https://help.elgato.com/hc/en-us/articles/360027960912-Elgato-Stream-Deck-Multi-Actions). These describe the interaction. The static inspection establishes which structures and components were shipped.

## Reproduce

Use REA 6.8.0 against the extracted `SourceDir/PFiles64/Elgato/StreamDeck` folder. Set `REA_RIZIN_COMMAND` to the local Rizin executable for the string queries.

```powershell
$app = 'C:\path\to\extracted\SourceDir\PFiles64\Elgato\StreamDeck'
$exe = Join-Path $app 'StreamDeck.exe'
rea inspect-pe-imports $exe --json
rea execute-rizin-command $exe 'izz~PropertyInspector' --json
rea execute-rizin-command $exe 'izz~ActionList' --json
rea inspect-artifact (Join-Path $app 'DefaultProfiles\StreamDeckMini_winDefault.streamDeckProfile') --json
```
