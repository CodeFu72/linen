#!/usr/bin/env bash
# Builds Linen and installs it for you: the app, its icon, and a launcher entry.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release
install -Dm755 target/release/linen "$HOME/.local/bin/linen"
install -Dm644 ui/linen.svg "$HOME/.local/share/icons/hicolor/scalable/apps/local.kirk.Linen.svg"
cat > "$HOME/.local/share/applications/local.kirk.Linen.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=Linen
Comment=Soft, clean notes with spelling and a helpful right-click
Exec=$HOME/.local/bin/linen %F
Icon=local.kirk.Linen
Terminal=false
Categories=Utility;TextEditor;
MimeType=text/plain;text/markdown;
StartupWMClass=local.kirk.Linen
Keywords=notes;text;write;editor;passwords;
DESK
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
echo "Installed Linen. Open it from your app launcher, or run: linen"
