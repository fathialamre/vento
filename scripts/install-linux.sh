#!/bin/sh
# Builds vento in release mode and installs it for the current user (GNOME on
# Wayland): the binary in ~/.local/bin, its .desktop file and icon, and its
# GNOME Shell extension, which it needs to see the pointer, drags and the
# clipboard; Vento then starts at every login. `--no-autostart` leaves the
# login out, `--uninstall` removes all of it.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

app_id=io.github.fathialamre.Vento
uuid=vento@fathialamre.github.io
bin_dir=$HOME/.local/bin
data=${XDG_DATA_HOME:-$HOME/.local/share}
desktop=$data/applications/$app_id.desktop
icon=$data/icons/hicolor/scalable/apps/$app_id.svg
extension=$data/gnome-shell/extensions/$uuid
autostart=${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$app_id.desktop

# Adds the extension to GNOME's enabled list, or removes it (`remove`).
set_enabled() {
    list=$(gsettings get org.gnome.shell enabled-extensions)
    others=$(printf '%s' "$list" | sed -e 's/^@as //' -e "s/'$uuid'//" \
        -e 's/\[, */[/' -e 's/, *\]/]/' -e 's/, *,/,/')
    if [ "$1" = remove ]; then
        new=$others
    elif [ "$others" = "[]" ]; then
        new="['$uuid']"
    else
        new="${others%]}, '$uuid']"
    fi
    gsettings set org.gnome.shell enabled-extensions "$new"
}

refresh_caches() {
    update-desktop-database "$data/applications" 2>/dev/null || true
    gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = --uninstall ]; then
    gnome-extensions disable "$uuid" 2>/dev/null || true
    set_enabled remove
    rm -rf "$extension"
    rm -f "$bin_dir/vento" "$desktop" "$icon" "$autostart"
    refresh_caches
    echo "Vento removed. Its settings stay in ~/.config/vento and its data in $data/vento."
    exit 0
fi

if ! ldconfig -p 2>/dev/null | grep -q 'libxkbcommon-x11\.so\.0'; then
    echo "Vento needs libxkbcommon-x11 to run: sudo apt install libxkbcommon-x11-0" >&2
fi

cargo build -p vento --release

install -Dm755 target/release/vento "$bin_dir/vento"
install -Dm644 crates/vento/assets/vento.svg "$icon"
mkdir -p "$data/applications"
sed "s|^Exec=.*|Exec=$bin_dir/vento|" "crates/vento/assets/$app_id.desktop" > "$desktop"
refresh_caches

# Started at login like any other app: GNOME lists it among the startup
# apps, where it can be turned off.
if [ "${1:-}" = --no-autostart ]; then
    rm -f "$autostart"
else
    mkdir -p "$(dirname "$autostart")"
    { cat "$desktop"; echo "X-GNOME-Autostart-enabled=true"; } > "$autostart"
fi

was_active=no
gnome-extensions info "$uuid" 2>/dev/null | grep -q 'State: ACTIVE' && was_active=yes
changed=yes
if [ -d "$extension" ] && diff -rq "gnome-shell/$uuid" "$extension" >/dev/null; then
    changed=no
fi
rm -rf "$extension"
mkdir -p "$(dirname "$extension")"
cp -r "gnome-shell/$uuid" "$extension"
# GNOME knows the extension when it was there at login; a new one is only
# picked up at the next login, from the enabled list.
gnome-extensions enable "$uuid" 2>/dev/null || set_enabled add

echo "Vento installed in $bin_dir/vento; start it from the app grid."
[ -f "$autostart" ] && echo "It starts at every login ($autostart)."

if [ "$was_active" = no ]; then
    echo "Log out and back in once, so GNOME Shell loads Vento's extension."
elif [ "$changed" = yes ]; then
    echo "Vento's extension changed: log out and back in to load the new version."
fi
