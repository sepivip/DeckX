"""Exercise the published Windows setup on a disposable GitHub runner."""
import ctypes
from ctypes import wintypes
from hashlib import file_digest
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from urllib.request import Request, urlopen


VERSION = '0.7.13'
SETUP_NAME = f'DeckX-{VERSION}-windows-x64-setup.exe'
SETUP_SHA256 = '8ccbe2a5dec7f291a9495c81cb3f5639d0f2987ae0bc19d5ee0b5079d53cc646'
RELEASE = f'https://github.com/sepivip/DeckX/releases/download/v{VERSION}'
MESA_VERSION = '26.3.1'
MESA_NAME = f'mesa3d-{MESA_VERSION}-release-msvc.7z'
MESA_RELEASE = f'https://github.com/pal1000/mesa-dist-win/releases/download/{MESA_VERSION}'
MESA_SHA256 = '64e84cf13d67dd354640bf443ed0445e6a83afd06a5729f0750b3744d633b3f1'
user32 = ctypes.WinDLL('user32', use_last_error=True)
user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.FindWindowW.restype = wintypes.HWND
user32.IsWindowVisible.argtypes = [wintypes.HWND]
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


def download_verified(url, path, checksum):
    with urlopen(Request(url, headers={'User-Agent': 'DeckX Windows test'}), timeout=30) as response:
        with path.open('wb') as output:
            shutil.copyfileobj(response, output)
    with path.open('rb') as downloaded:
        assert file_digest(downloaded, 'sha256').hexdigest() == checksum, f'{path.name} checksum mismatch'


def launch_from_start(shortcut, marker):
    os.startfile(str(shortcut), 'open')
    deadline = time.monotonic() + 22
    window = None
    while time.monotonic() < deadline:
        window = user32.FindWindowW(None, 'DeckX')
        if window and user32.IsWindowVisible(window) and marker.is_file():
            return window
        time.sleep(.25)
    print(f'Start menu launch timed out (window={bool(window)}, MCP ready={marker.is_file()}).', flush=True)
    return None


def startup_error(executable):
    try:
        result = subprocess.run([executable], cwd=executable.parent, capture_output=True, text=True,
                                errors='replace', timeout=8, creationflags=subprocess.CREATE_NO_WINDOW)
    except subprocess.TimeoutExpired:
        print('Direct launch was still running after eight seconds.', flush=True)
        return ''
    error = (result.stderr + result.stdout).strip()
    print(f'Direct launch exited with code {result.returncode}. {error}', flush=True)
    return error


def install_vm_software_gl(temp, directory):
    archive = temp / MESA_NAME
    download_verified(f'{MESA_RELEASE}/{MESA_NAME}', archive, MESA_SHA256)
    source = temp / 'mesa'
    subprocess.run(['7z', 'x', archive, f'-o{source}', '-y', 'x64\\*.dll'], check=True, timeout=40)
    for driver in ('opengl32', 'opengl32sw', 'libgallium_wgl', 'libglapi'):
        filename = f'{driver}.dll'
        dll = source / 'x64' / filename
        if driver in ('opengl32', 'libgallium_wgl'):
            assert dll.is_file(), f'Missing {filename} in the Mesa distribution'
        if dll.is_file():
            shutil.copyfile(dll, directory / filename)
    os.environ['GALLIUM_DRIVER'] = 'llvmpipe'
    print('Enabled software OpenGL for DeckX on the test VM.', flush=True)


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
    download_verified(f'{RELEASE}/{SETUP_NAME}', setup, SETUP_SHA256)
    subprocess.run([setup, '/SP-', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCLOSEAPPLICATIONS'],
                   check=True, timeout=75, creationflags=subprocess.CREATE_NO_WINDOW)
    assert shortcut.is_file() and executable.is_file(), 'Setup did not create the Start menu entry and executable'
    assert profile.read_bytes() == previous, 'Installer modified the previously saved profile'
    print('Verified the published installer and its Start menu shortcut.', flush=True)

    marker = profile.with_name('mcp-endpoint.json')
    window = launch_from_start(shortcut, marker)
    if window is None:
        error = startup_error(executable)
        if any(fragment in error.casefold() for fragment in ('glutin', 'opengl', 'wgl', 'egl')):
            install_vm_software_gl(temp, executable.parent)
            window = launch_from_start(shortcut, marker)
            if window is None:
                startup_error(executable)
    try:
        assert window, 'The Start menu shortcut did not open an initialized DeckX window'
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
