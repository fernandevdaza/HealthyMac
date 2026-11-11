#!/bin/zsh
# Regenerates the app icon from scripts/make_icon.swift:
#   crates/hm-app/assets/AppIcon-light.png and AppIcon-dark.png, which the app shows in the Dock and
#   the sidebar to match light or dark mode, and AppIcon.icns, the bundle's icon for Finder and
#   Launchpad, made from the light variant.
set -eu
root="${0:A:h:h}"
work="$(mktemp -d)"
trap 'rm -r "$work"' EXIT

swift "$root/scripts/make_icon.swift" "$work/light.png" light
swift "$root/scripts/make_icon.swift" "$work/dark.png" dark

set_dir="$work/AppIcon.iconset"
mkdir "$set_dir"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$work/light.png" --out "$set_dir/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$work/light.png" --out "$set_dir/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$root/crates/hm-app/assets"
iconutil -c icns "$set_dir" -o "$root/crates/hm-app/assets/AppIcon.icns"
cp "$work/light.png" "$root/crates/hm-app/assets/AppIcon-light.png"
cp "$work/dark.png" "$root/crates/hm-app/assets/AppIcon-dark.png"
echo "$root/crates/hm-app/assets/AppIcon.icns"
