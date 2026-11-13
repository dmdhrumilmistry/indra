#!/usr/bin/env python3
"""Mirror the canonical skills into the layouts other agents discover.

Canonical source of truth:  plugins/indra/skills/
  (that path is fixed by the Claude Code plugin format)

Generated mirrors:
  skills/                  Gemini CLI loads an extension's skills from
                           <extension root>/skills, and the extension root is
                           the repo root, so the tree has to exist there too.

Optional install targets (--install), for agents that read a well-known dir:
  ~/.agents/skills/        the agentskills.io interoperable path. opencode and
                           Gemini CLI both discover it, as does anything else
                           following that standard.
  ~/.claude/skills/        Claude Code user skills, for people who would rather
                           not install the plugin.
  .agents/skills/          same standard, scoped to one workspace.

Usage:
  python scripts/sync-agent-skills.py            # refresh the in-repo mirror
  python scripts/sync-agent-skills.py --check    # verify it is current (CI)
  python scripts/sync-agent-skills.py --install agents
  python scripts/sync-agent-skills.py --install claude --scope user
"""

import argparse
import filecmp
import os
import shutil
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(REPO, "plugins", "indra", "skills")
MIRROR = os.path.join(REPO, "skills")

BANNER = (
    "<!-- GENERATED FILE. Edit the copy under plugins/indra/skills/ "
    "and run scripts/sync-agent-skills.py. -->\n"
)


def tree_files(root):
    """Every file under root, as paths relative to root."""
    out = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d != "__pycache__"]
        for f in filenames:
            full = os.path.join(dirpath, f)
            out.append(os.path.relpath(full, root))
    return sorted(out)


def in_sync(source, mirror):
    """True when mirror is a byte-identical copy of source."""
    if not os.path.isdir(mirror):
        return False, ["mirror does not exist: %s" % mirror]
    src, dst = tree_files(source), tree_files(mirror)
    problems = []
    for missing in sorted(set(src) - set(dst)):
        problems.append("missing from mirror: %s" % missing)
    for extra in sorted(set(dst) - set(src)):
        problems.append("stale in mirror: %s" % extra)
    for rel in sorted(set(src) & set(dst)):
        a, b = os.path.join(source, rel), os.path.join(mirror, rel)
        if not filecmp.cmp(a, b, shallow=False):
            problems.append("differs: %s" % rel)
    return (not problems), problems


def copy_tree(source, dest):
    if os.path.isdir(dest):
        shutil.rmtree(dest)
    shutil.copytree(source, dest)
    _mark_executable(dest)
    return len(tree_files(dest))


def _mark_executable(root):
    """Give every bundled script the exec bit.

    Windows has no exec bit, so a mirror generated there would otherwise be
    committed 0644 and land non-executable for everyone who installs it on Linux
    or macOS, where the shebang is the point. Setting it here means the mirror
    cannot drift from the canonical copy depending on who regenerated it.
    """
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if not f.endswith((".py", ".sh")):
                continue
            p = os.path.join(dirpath, f)
            try:
                mode = os.stat(p).st_mode
                os.chmod(p, mode | 0o111)
            except OSError:
                pass


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true",
                    help="exit non-zero if the in-repo mirror is stale")
    ap.add_argument("--install", choices=["agents", "claude", "opencode"],
                    help="also copy the skills into a discovery directory")
    ap.add_argument("--scope", choices=["user", "workspace"], default="user",
                    help="install into the home directory or this workspace")
    args = ap.parse_args()

    if not os.path.isdir(SOURCE):
        print("error: canonical skills not found at %s" % SOURCE, file=sys.stderr)
        return 2

    if args.check:
        ok, problems = in_sync(SOURCE, MIRROR)
        if ok:
            print("skills/ mirror is up to date (%d files)." % len(tree_files(SOURCE)))
            return 0
        print("skills/ mirror is STALE. Run: python scripts/sync-agent-skills.py",
              file=sys.stderr)
        for p in problems:
            print("  " + p, file=sys.stderr)
        return 1

    n = copy_tree(SOURCE, MIRROR)
    print("Synced %d files -> %s" % (n, os.path.relpath(MIRROR, REPO)))

    if args.install:
        home = os.path.expanduser("~")
        roots = {
            "agents": os.path.join(home, ".agents", "skills") if args.scope == "user"
                      else os.path.join(os.getcwd(), ".agents", "skills"),
            "claude": os.path.join(home, ".claude", "skills") if args.scope == "user"
                      else os.path.join(os.getcwd(), ".claude", "skills"),
            "opencode": os.path.join(home, ".config", "opencode", "skills")
                        if args.scope == "user"
                        else os.path.join(os.getcwd(), ".opencode", "skills"),
        }
        dest_root = roots[args.install]
        os.makedirs(dest_root, exist_ok=True)
        count = 0
        for name in sorted(os.listdir(SOURCE)):
            src = os.path.join(SOURCE, name)
            if not os.path.isdir(src):
                continue
            copy_tree(src, os.path.join(dest_root, name))
            count += 1
        print("Installed %d skills -> %s" % (count, dest_root))

    return 0


if __name__ == "__main__":
    sys.exit(main())
