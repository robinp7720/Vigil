#!/usr/bin/env python3
"""Live Hyprland regression: python tests/startup_lifecycle.py target/release/vigil.

Uses private D-Bus, IPC and state directories. Temporarily adds test bar surfaces
on the current displays; never stops the installed service or changes outputs.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

binary = Path(sys.argv[1]).resolve()
runtime = Path(os.environ['XDG_RUNTIME_DIR'])
display = Path(os.environ['WAYLAND_DISPLAY'])
if not display.is_absolute():
    display = runtime / display
hyprctl = shutil.which('hyprctl')
assert hyprctl and os.environ.get('HYPRLAND_INSTANCE_SIGNATURE')

with tempfile.TemporaryDirectory(prefix='vs-') as directory:
    root = Path(directory)
    (root / 'hypr').symlink_to(runtime / 'hypr')
    (root / 'bin').mkdir()
    wrapper = root / 'bin/hyprctl'
    wrapper.write_text('#!/bin/sh\n'
                       'case " $* " in *" monitors "*) sleep 3;; esac\n'
                       'export XDG_RUNTIME_DIR="$VIGIL_TEST_RUNTIME"\n'
                       'exec "$VIGIL_TEST_HYPRCTL" "$@"\n')
    wrapper.chmod(0o700)
    config = root / 'config.toml'
    config.write_text('')
    env = dict(os.environ, XDG_RUNTIME_DIR=directory,
               XDG_STATE_HOME=str(root / 'state'), XDG_CACHE_HOME=str(root / 'cache'),
               WAYLAND_DISPLAY=str(display), NIRI_SOCKET='',
               VIGIL_TEST_RUNTIME=str(runtime), VIGIL_TEST_HYPRCTL=hyprctl,
               PATH=str(root / 'bin') + ':' + os.environ['PATH'])
    with (root / 'log').open('w+') as log:
        process = subprocess.Popen(
            ['dbus-run-session', '--', str(binary), '--config', str(config)],
            env=env, stdout=log, stderr=log, start_new_session=True)
        try:
            # Wait past both delayed discovery and the former clean shutdown.
            time.sleep(7)
            assert process.poll() is None, 'Vigil exited while waiting for its first outputs'
            layers = json.loads(subprocess.check_output([hyprctl, 'layers', '-j']))
            own_pids = []
            for item in Path('/proc').iterdir():
                if item.name.isdigit():
                    try:
                        if os.getpgid(int(item.name)) == process.pid:
                            own_pids.append(int(item.name))
                    except ProcessLookupError:
                        pass
            mapped = [name for name, output in layers.items()
                      if any(layer['pid'] in own_pids and layer['namespace'] == 'vigil'
                             for level in output['levels'].values() for layer in level)]
            assert mapped, 'Vigil stayed alive but did not create its delayed surfaces'
            log.flush()
            log.seek(0)
            assert 'New application windows must be added' not in log.read()
            print('PASS: survived delayed discovery and mapped bars on ' + ', '.join(mapped))
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read(), file=sys.stderr)
            raise
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            process.wait(timeout=10)
