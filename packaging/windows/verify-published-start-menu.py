"""Exercise the published Windows setup on a disposable GitHub runner."""
import ctypes
from ctypes import wintypes
from hashlib import sha256
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.request import Request, urlopen


VERSION = '0.7.13'
SETUP_NAME = f'DeckX-{VERSION}-windows-x64-setup.exe'
SETUP_SHA256 = '8ccbe2a5dec7f291a9495c81cb3f5639d0f2987ae0bc19d5ee0b5079d53cc646'
RELEASE = f'https://github.com/sepivip/DeckX/releases/download/v{VERSION}'
user32 = ctypes.WinDLL('user32', use_last_error=True)
user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.FindWindowW.restype = wintypes.HWND
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]


def saved_fixture():
    def action(kind='None', value=''):
        return {'kind': kind, 'value': value}

    def page(page_id, name, label, kind, value):
        buttons = [{'label': '', 'image': None, 'action': action()} for _ in range(9)]
        buttons[2] = {'label': label, 'image': None, 'color': [30, 72, 156], 'action': action(kind, value)}
        layout = {'buttons': buttons, 'dials': [[action() for _ in range(3)] for _ in range(3)]}
        return {'id': page_id, 'name': name, 'folder': False, 'layout': layout}

    return {
        'version': 2, 'brightness': 47, 'rotation': 90, 'easter_egg': False, 'minimize_to_tray': False,
        'home': {'profile': 17, 'page': 26},
        'profiles': [{'id': 17, 'name': 'Installer test', 'pages': [
            page(25, 'First page', 'Mute', 'Mute', ''),
            page(26, 'Saved home', 'Docs', 'OpenWebsite', 'https://example.com/docs'),
        ]}],
    }


def deckx_state(executable):
    query = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
             'params': {'name': 'deckx_state', 'arguments': {}}}
    reply = subprocess.run([executable, '--mcp'], input=json.dumps(query) + '\n', text=True,
                           capture_output=True, check=True, timeout=15,
                           creationflags=subprocess.CREATE_NO_WINDOW)
    return json.loads(reply.stdout)['result']['structuredContent']


def main():
    temp = Path(os.environ['RUNNER_TEMP'])
    local_data = Path(os.environ['LOCALAPPDATA'])
    profile = local_data / 'DeckX' / 'profile.json'
    executable = local_data / 'Programs' / 'DeckX' / 'deckx.exe'
    shortcut = Path(os.environ['APPDATA']) / 'Microsoft/Windows/Start Menu/Programs/DeckX.lnk'
    assert not any(path.exists() for path in (profile, executable, shortcut)), 'Use a fresh Windows runner'

    workspace = saved_fixture()
    previous = json.dumps(workspace, indent=2).encode()
    profile.parent.mkdir(parents=True, exist_ok=True)
    profile.write_bytes(previous)
    setup = temp / SETUP_NAME
    with urlopen(Request(f'{RELEASE}/{SETUP_NAME}', headers={'User-Agent': 'DeckX Windows test'}), timeout=30) as response:
        payload = response.read()
    assert sha256(payload).hexdigest() == SETUP_SHA256, 'Published setup checksum mismatch'
    setup.write_bytes(payload)
    subprocess.run([setup, '/SP-', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCLOSEAPPLICATIONS'],
                   check=True, timeout=75, creationflags=subprocess.CREATE_NO_WINDOW)
    assert shortcut.is_file() and executable.is_file(), 'Setup did not create the Start menu entry and executable'
    assert profile.read_bytes() == previous, 'Installer modified the previously saved profile'
    print('Verified the published installer and its Start menu shortcut.', flush=True)

    os.startfile(str(shortcut), 'open')
    marker = profile.with_name('mcp-endpoint.json')
    window = None
    try:
        deadline = time.monotonic() + 32
        while time.monotonic() < deadline:
            window = user32.FindWindowW(None, 'DeckX')
            if window and marker.is_file():
                break
            time.sleep(.25)
        assert window and marker.is_file(), 'The Start menu shortcut did not open an initialized DeckX window'
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(window, ctypes.byref(pid))
        image = subprocess.check_output(
            ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', f'(Get-Process -Id {pid.value}).Path'],
            text=True, timeout=10, creationflags=subprocess.CREATE_NO_WINDOW,
        ).strip()
        assert Path(image).resolve() == executable.resolve(), 'The Start menu opened a different binary'

        state = deckx_state(executable)
        assert state['version'] == VERSION and not state['error'], state.get('message')
        assert state['editor_page'] == workspace['home'], 'DeckX selected a different saved page'
        assert state['workspace'] == workspace, 'DeckX did not load the saved workspace and assignments'
        assert profile.read_bytes() == previous, 'Opening DeckX changed the saved profile'
        print('The Start menu opened the installed DeckX 0.7.13 window on its saved home page.', flush=True)
        print('Both pages, LCD assignments, and display settings matched the saved workspace.', flush=True)
    finally:
        if window:
            user32.PostMessageW(window, 0x0010, 0, 0)


if __name__ == '__main__':
    main()
