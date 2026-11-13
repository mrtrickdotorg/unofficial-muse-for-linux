#!/usr/bin/env python3
"""Rewrite WebKitGTK's compiled-in helper-process directory.

Debian's WebKitGTK looks for WebKitNetworkProcess and friends at
the absolute path it was built with. Inside an AppImage that path
does not exist, and the first WebView is fatal — the whole app
traps, as the first laptop run of the WebView feed proved. The
replacement is a fixed staging path the app itself points at the
bundle's helpers with a symlink on every launch (see main.rs),
so every process WebKit spawns finds them.

Longer forms are replaced first so a suffixed path keeps its
suffix; replacements are NUL-padded to the original length so no
ELF offset moves. The build fails unless libwebkitgtk itself was
patched and no occurrence of the original path survives.
"""
import os
import sys

FORMS = [
    (b"/usr/lib/x86_64-linux-gnu/webkitgtk-6.0/injected-bundle/",
     b"/tmp/.webkitgtk-6.0-umfl/injected-bundle/"),
    (b"/usr/lib/x86_64-linux-gnu/webkitgtk-6.0",
     b"/tmp/.webkitgtk-6.0-umfl"),
]


def patch_file(path):
    with open(path, "rb") as handle:
        data = handle.read()
    total = 0
    for old, new in FORMS:
        padded = new + b"\0" * (len(old) - len(new))
        hits = data.count(old)
        if hits:
            data = data.replace(old, padded)
            total += hits
    if total:
        with open(path, "wb") as handle:
            handle.write(data)
    return total


def files_under(root):
    seen_inodes = set()
    for dirpath, _dirs, files in os.walk(root):
        for name in files:
            path = os.path.join(dirpath, name)
            try:
                stat = os.stat(path)
            except OSError:
                continue
            if stat.st_ino in seen_inodes:
                continue
            seen_inodes.add(stat.st_ino)
            yield path


def main(root):
    patched = {}
    for path in files_under(root):
        try:
            hits = patch_file(path)
        except (OSError, PermissionError):
            continue
        if hits:
            patched[path] = hits
    for path, hits in sorted(patched.items()):
        print(f"{hits} x {path}")
    print(f"total replacements: {sum(patched.values())}")
    if not any("libwebkitgtk" in os.path.basename(p) for p in patched):
        print("libwebkitgtk was not patched — refusing to package", file=sys.stderr)
        return 1
    for path in files_under(root):
        try:
            with open(path, "rb") as handle:
                data = handle.read()
        except (OSError, PermissionError):
            continue
        if any(old in data for old, _new in FORMS):
            print(f"original helper path survives in {path}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
