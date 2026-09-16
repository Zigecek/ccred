#!/usr/bin/env python3
"""Make the installers' "now put it on PATH" advice copy-pasteable.

cargo-dist prints the shell name on the same line as the command:

    source $HOME/.local/bin/env (sh, bash, zsh)

Someone following that selects the whole line, parentheses and all, and the
shell then tries to run a file called `(sh,`. It happened to a real person on
their first install. The command gets a line of its own here, under a heading
that names the shell.

The generated scripts are not ours to edit in the repository -- `dist` rewrites
them on every release -- so this patches the published assets instead, and
fails loudly if the text it expects is gone, which is how a template change
gets noticed rather than silently shipped.
"""

import sys
from pathlib import Path

SH_OLD = '''            say "To add $_install_dir_expr to your PATH, either restart your shell or run:"
            say ""
            say "    source $_env_script_path_expr (sh, bash, zsh)"
            say "    source $_fish_env_script_path_expr (fish)"
'''

SH_NEW = '''            say "$APP_NAME is installed in $_install_dir_expr, which this shell does not know about yet."
            say "Open a new terminal, or copy the single line under your shell's name:"
            say ""
            say "  bash, zsh or sh"
            say "    source $_env_script_path_expr"
            say ""
            say "  fish"
            say "    source $_fish_env_script_path_expr"
'''

PS_OLD = '''        Write-Information "To add $dest_dir to your PATH, either restart your shell or run:"
        Write-Information ""
        Write-Information "    set Path=$dest_dir;%Path%   (cmd)"
        Write-Information "    `$env:Path = `"$dest_dir;`$env:Path`"   (powershell)"
'''

PS_NEW = '''        Write-Information "$app_name is installed in $dest_dir, which this window does not know about yet."
        Write-Information "Open a new terminal, or copy the single line under your shell's name:"
        Write-Information ""
        Write-Information "  PowerShell"
        Write-Information "    `$env:Path = `"$dest_dir;`$env:Path`""
        Write-Information ""
        Write-Information "  cmd"
        Write-Information "    set Path=$dest_dir;%Path%"
'''

PATCHES = {".sh": (SH_OLD, SH_NEW), ".ps1": (PS_OLD, PS_NEW)}


def patch(path: Path) -> None:
    old, new = PATCHES[path.suffix]
    text = path.read_text(encoding="utf-8")
    if new in text:
        print(f"{path.name}: already clear")
        return
    if text.count(old) != 1:
        sys.exit(
            f"{path.name}: the PATH advice is not where it was "
            f"({text.count(old)} matches). dist changed its template: update "
            f"scripts/clarify-installer.py to match, then re-run."
        )
    path.write_text(text.replace(old, new), encoding="utf-8", newline="")
    print(f"{path.name}: PATH advice rewritten")


def main(argv: list[str]) -> None:
    if len(argv) < 2:
        sys.exit("usage: clarify-installer.py <installer.sh|installer.ps1>...")
    for arg in argv[1:]:
        path = Path(arg)
        if path.suffix not in PATCHES:
            sys.exit(f"{path.name}: not an installer this script knows")
        patch(path)


if __name__ == "__main__":
    main(sys.argv)
