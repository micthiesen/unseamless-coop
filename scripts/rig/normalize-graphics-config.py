#!/usr/bin/env python3
"""Normalize Elden Ring's saved GraphicsConfig.xml to fullscreen at the gaming resolution.

Why this exists: rig runs launch the game inside a small gamescope (1440x900 windowed), and the
game persists what it sees — it rewrites GraphicsConfig.xml to WINDOW mode with its resolutions
clamped to that little display. The next manual (fullscreen) launch then renders a 720p-ish window
buffer that gamescope upscales to the whole screen: blurry. gamescope-wrapper.sh calls this on the
GAMING path (never the rig path) so every manual Play starts from FULLSCREEN at native res, no
matter what the last rig session left behind.

The file is UTF-16 (as the game writes it); rewrite preserves that encoding. No-op when the values
are already right. Any failure is non-fatal to the caller (the wrapper launches anyway).
"""
import os
import re
import sys

PATH = os.environ.get(
    "UNSEAMLESS_ER_GRAPHICS_CONFIG",
    "/mnt/games/SteamLibrary/steamapps/compatdata/1245620/pfx/drive_c/users/steamuser"
    "/AppData/Roaming/EldenRing/GraphicsConfig.xml",
)
W = os.environ.get("UNSEAMLESS_GAMING_WIDTH", "3440")
H = os.environ.get("UNSEAMLESS_GAMING_HEIGHT", "1440")

SUBS = [
    (r"<ScreenMode>[^<]*</ScreenMode>", "<ScreenMode>FULLSCREEN</ScreenMode>"),
    (r"<Resolution-FullScreenWidth>[^<]*</Resolution-FullScreenWidth>",
     f"<Resolution-FullScreenWidth>{W}</Resolution-FullScreenWidth>"),
    (r"<Resolution-FullScreenHeight>[^<]*</Resolution-FullScreenHeight>",
     f"<Resolution-FullScreenHeight>{H}</Resolution-FullScreenHeight>"),
    (r"<Resolution-BorderlessScreenWidth>[^<]*</Resolution-BorderlessScreenWidth>",
     f"<Resolution-BorderlessScreenWidth>{W}</Resolution-BorderlessScreenWidth>"),
    (r"<Resolution-BorderlessScreenHeight>[^<]*</Resolution-BorderlessScreenHeight>",
     f"<Resolution-BorderlessScreenHeight>{H}</Resolution-BorderlessScreenHeight>"),
]

try:
    text = open(PATH, "rb").read().decode("utf-16")
except OSError as e:
    sys.exit(f"normalize-graphics-config: cannot read {PATH}: {e}")

new = text
for pat, rep in SUBS:
    new = re.sub(pat, rep, new)

if new == text:
    print("normalize-graphics-config: already fullscreen "
          f"{W}x{H}, nothing to do")
else:
    open(PATH, "wb").write(new.encode("utf-16"))
    print(f"normalize-graphics-config: reset to FULLSCREEN {W}x{H} (rig run had clamped it)")
