#!/usr/bin/env python3
"""Read-only drift detector for the /sync skill (see SKILL.md next to this file).

Usage:
  sync-status.py                   report per-peer, per-resource sync status
  sync-status.py diff PEER PATH    token-normalized unified diff for one
                                   resource (PATH as written in sync-map.json,
                                   or SKILL.md / sync-status.py for the skill
                                   itself)

Reads sync-map.json next to this script. Never writes anything.
Exit codes: 0 = no mechanical drift, 1 = exact-mode drift or missing files,
2 = usage/config error.
"""

import difflib
import json
import os
import re
import sys
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent
SKIP_NAMES = {".git", "__pycache__", ".DS_Store", "node_modules", "target"}
# The skill's own files, implicitly shared with every peer. SKILL.md is
# compared body-only (frontmatter legitimately differs per environment);
# sync-map.json is per-project and never compared.
SELF_FILES = ("SKILL.md", "sync-status.py")


def fail(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(2)


def load():
    map_path = SKILL_DIR / "sync-map.json"
    if not map_path.exists():
        fail(
            f"no sync-map.json in {SKILL_DIR} - this project isn't set up "
            "for /sync; see SKILL.md 'Bootstrapping a new project'"
        )
    try:
        m = json.loads(map_path.read_text())
    except (json.JSONDecodeError, OSError) as e:
        fail(f"unreadable sync-map.json ({map_path}): {e}")
    root = Path(m["root"]).expanduser() if m.get("root") else SKILL_DIR.parents[2]
    return m, root.resolve()


def peer_root(root, cfg):
    p = Path(cfg["path"]).expanduser()
    return (p if p.is_absolute() else root / p).resolve()


def peer_skill_dir(proot, cfg):
    sub = ".rulesync/skills/sync" if cfg.get("layout") == "rulesync" else ".claude/skills/sync"
    return proot / sub


def apply_tokens(text, tokens):
    if not tokens:
        return text
    # Single-pass simultaneous substitution: sequential str.replace would let
    # one replacement's output be mangled by a later token.
    pattern = re.compile("|".join(re.escape(k) for k in sorted(tokens, key=len, reverse=True)))
    return pattern.sub(lambda mo: tokens[mo.group(0)], text)


def strip_frontmatter(text):
    if text.startswith("---\n"):
        end = text.find("\n---\n", 4)
        if end != -1:
            # lstrip: generators (rulesync) may drop the blank line after the
            # frontmatter block; that's environment noise, not drift.
            return text[end + 5 :].lstrip("\n")
    return text


def read_text(path):
    try:
        return path.read_text()
    except UnicodeDecodeError:
        return None


def file_same(local, peer, tokens, transform=None):
    """True if peer content == token-mapped local content."""
    l_dangling = local.is_symlink() and not local.exists()
    p_dangling = peer.is_symlink() and not peer.exists()
    if l_dangling or p_dangling:
        if l_dangling and p_dangling:
            return apply_tokens(os.readlink(local), tokens) == os.readlink(peer)
        return False
    lt, pt = read_text(local), read_text(peer)
    if lt is None or pt is None:  # binary: raw compare, no token mapping
        return local.read_bytes() == peer.read_bytes()
    if transform:
        lt, pt = transform(lt), transform(pt)
    return apply_tokens(lt, tokens) == pt


def iter_files(base):
    for p in sorted(base.rglob("*")):
        rel = p.relative_to(base)
        if any(part in SKIP_NAMES for part in rel.parts):
            continue
        if p.is_file() or (p.is_symlink() and not p.exists()):
            yield rel


def compare(local, peer, tokens, transform=None):
    """-> (status, detail); status: ok | drift | missing-here | missing-peer | missing-both"""
    lex, pex = local.exists(), peer.exists()
    if not lex and not pex:
        return "missing-both", ""
    if not lex:
        return "missing-here", ""
    if not pex:
        return "missing-peer", ""
    if local.is_dir() != peer.is_dir():
        return "drift", "file vs directory"
    if local.is_dir():
        lf, pf = set(iter_files(local)), set(iter_files(peer))
        details = [f"{rel}: missing on peer" for rel in sorted(lf - pf)]
        details += [f"{rel}: missing here" for rel in sorted(pf - lf)]
        details += [
            f"{rel}: differs"
            for rel in sorted(lf & pf)
            if not file_same(local / rel, peer / rel, tokens, transform)
        ]
        return ("drift", "; ".join(details)) if details else ("ok", "")
    return ("ok", "") if file_same(local, peer, tokens, transform) else ("drift", "")


def resolve_resource(root, res, peer_name, proot):
    local = root / res["path"]
    peer_path = (res.get("peerPath") or {}).get(peer_name, res["path"])
    return local, proot / peer_path


def safe_compare(local, peer, tokens, transform=None):
    try:
        return compare(local, peer, tokens, transform)
    except OSError as e:
        return "error", str(e)


def report(status, mode, label, detail, notes=None):
    """Print one status line; return True if it should fail the run."""
    if status == "ok":
        word, fails = "ok", False
    elif status == "error":
        word, fails = "ERROR", True  # unreadable content is a problem in any mode
    elif mode == "judgment":
        word, fails = "review", False  # judgment resources never fail mechanically
    elif status == "drift":
        word, fails = "DRIFT", True
    else:
        word, fails = status.upper(), True
    line = f"  {word:<13} {label}"
    if detail:
        line += f"  [{detail}]"
    print(line)
    if notes and word != "ok":
        print(f"{'':15}note: {notes}")
    return fails


def cmd_status(m, root):
    bad = False
    for name, cfg in m.get("peers", {}).items():
        proot = peer_root(root, cfg)
        print(f"peer {name} ({proot})")
        if not proot.is_dir():
            print("  skipped       not checked out")
            continue
        pskill = peer_skill_dir(proot, cfg)
        for fname in SELF_FILES:
            transform = strip_frontmatter if fname == "SKILL.md" else None
            st, det = safe_compare(SKILL_DIR / fname, pskill / fname, cfg.get("tokens"), transform)
            bad |= report(st, "exact", f"sync skill: {fname}", det)
        for res in m.get("resources", []):
            if name not in res.get("peers", []):
                continue
            local, ppath = resolve_resource(root, res, name, proot)
            st, det = safe_compare(local, ppath, cfg.get("tokens"))
            bad |= report(st, res.get("mode", "exact"), res["path"], det, res.get("notes"))
    sys.exit(1 if bad else 0)


def show_diff(local, peer, tokens, transform=None):
    lt, pt = read_text(local), read_text(peer)
    if lt is None or pt is None:
        same = local.read_bytes() == peer.read_bytes()
        print(f"binary files {'identical' if same else 'differ'}: {local} vs {peer}")
        return
    if transform:
        lt, pt = transform(lt), transform(pt)
    lt = apply_tokens(lt, tokens)
    sys.stdout.writelines(
        difflib.unified_diff(
            lt.splitlines(keepends=True),
            pt.splitlines(keepends=True),
            fromfile=f"local(token-mapped): {local}",
            tofile=f"peer: {peer}",
        )
    )


def cmd_diff(m, root, peer_name, res_path):
    cfg = m.get("peers", {}).get(peer_name)
    if cfg is None:
        fail(f"unknown peer '{peer_name}' (peers: {', '.join(m.get('peers', {}))})")
    proot = peer_root(root, cfg)
    if not proot.is_dir():
        fail(f"peer '{peer_name}' not checked out at {proot}")
    tokens = cfg.get("tokens")
    transform = None
    if res_path in SELF_FILES:
        local = SKILL_DIR / res_path
        peer = peer_skill_dir(proot, cfg) / res_path
        if res_path == "SKILL.md":
            transform = strip_frontmatter
    else:
        res = next((r for r in m.get("resources", []) if r["path"] == res_path), None)
        if res is None:
            fail(f"no resource '{res_path}' in sync-map.json")
        local, peer = resolve_resource(root, res, peer_name, proot)
    if not local.exists() or not peer.exists():
        print(f"missing: local={local.exists()} peer={peer.exists()} ({local} vs {peer})")
        return
    if local.is_dir() and peer.is_dir():
        for rel in sorted(set(iter_files(local)) | set(iter_files(peer))):
            lp, pp = local / rel, peer / rel
            if not lp.exists() or not pp.exists():
                print(f"missing: local={lp.exists()} peer={pp.exists()} ({rel})")
            elif not file_same(lp, pp, tokens, transform):
                show_diff(lp, pp, tokens, transform)
    else:
        show_diff(local, peer, tokens, transform)


def main():
    m, root = load()
    args = sys.argv[1:]
    if not args:
        cmd_status(m, root)
    elif args[0] == "diff" and len(args) == 3:
        cmd_diff(m, root, args[1], args[2])
    else:
        fail("usage: sync-status.py [diff PEER PATH]")


if __name__ == "__main__":
    main()
