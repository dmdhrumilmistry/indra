#!/usr/bin/env python3
"""Local cache for sh-pr-review, so a re-push does not re-analyse unchanged code.

Why a script and not the model: a cache is exact bookkeeping, and a model that
hand-manages one will eventually report a hit it cannot justify. Every key here
is derived deterministically, and every miss states its reason.

Why it fails closed: a stale entry in a security tool does not make the tool
slow, it makes it *wrong*, by suppressing a finding the current knowledge base
would catch. So an entry is only ever returned when the knowledge bases, the
skill version, the model, and the file contents all still match what produced
it. Anything unrecognised is a miss.

Cache location, outside any repository so a temp clone can still use it:
  Windows  %LOCALAPPDATA%\\indra\\cache
  macOS    ~/Library/Caches/indra
  else     $XDG_CACHE_HOME/indra, or ~/.cache/indra

Commands:
  kb-version                       hash of every sh-kb-* knowledge base
  key                              the full key context, as JSON
  get     --repo R --pr N          the cached run, or a miss with its reason
  put     --repo R --pr N --file F store a run
  changed --repo R --pr N --file F which paths changed since the cached run
  stat    [--repo R]               what is cached and how big it is
  prune   [--max-age-days N]       drop expired entries
  clear   [--repo R]               drop everything, or one repo

Exit codes: 0 success (a miss is a success, it is an answer), 1 error,
2 bad usage.
"""

import argparse
import datetime as _dt
import hashlib
import io
import json
import os
import shutil
import sys

SCHEMA = 1
DEFAULT_TTL_DAYS = 7

HERE = os.path.dirname(os.path.abspath(__file__))


def _find_up(start, probe):
    """Walk up from `start` until `probe(dir)` is true. None if we hit the root.

    The script is bundled inside the skill so it travels with it, but the layout
    around that skill differs per agent: a Claude plugin, a Gemini extension, or
    a bare ~/.agents/skills tree. Discovering the knowledge bases by walking up
    keeps one copy of this file working in all of them.
    """
    d = start
    while True:
        if probe(d):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def _skills_dir():
    """The directory holding the sh-kb-* knowledge bases."""
    hit = _find_up(HERE, lambda d: any(
        n.startswith("sh-kb-") and os.path.isdir(os.path.join(d, n))
        for n in (os.listdir(d) if os.path.isdir(d) else [])))
    return hit


def _manifest():
    root = _find_up(HERE, lambda d: os.path.isfile(
        os.path.join(d, ".claude-plugin", "plugin.json")))
    if root:
        return os.path.join(root, ".claude-plugin", "plugin.json")
    root = _find_up(HERE, lambda d: os.path.isfile(os.path.join(d, "gemini-extension.json")))
    if root:
        return os.path.join(root, "gemini-extension.json")
    return None


SKILLS_DIR = _skills_dir()
MANIFEST = _manifest()


# --------------------------------------------------------------------------
# paths
# --------------------------------------------------------------------------

def cache_root():
    env = os.environ.get("SH_REVIEW_CACHE_DIR")
    if env:
        return os.path.join(env, "v%d" % SCHEMA)
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        return os.path.join(base, "indra", "cache", "v%d" % SCHEMA)
    if sys.platform == "darwin":
        return os.path.join(os.path.expanduser("~"), "Library", "Caches",
                            "indra", "v%d" % SCHEMA)
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    return os.path.join(base, "indra", "v%d" % SCHEMA)


def repo_slug(repo):
    """owner/repo -> a single safe directory name."""
    return repo.replace("/", "__").replace("\\", "__")


def entry_path(repo, pr):
    return os.path.join(cache_root(), repo_slug(repo), "pr-%s.json" % pr)


# --------------------------------------------------------------------------
# key material
# --------------------------------------------------------------------------

def kb_version():
    """A hash over every knowledge base that a hunter could load.

    This is the invalidation that matters most. The knowledge bases are updated
    on a schedule, and a cached 'this file is clean for sqli' produced before an
    update must never suppress a finding the new base would catch. Hashing the
    bases themselves means any KB change invalidates every cached finding, with
    no version number for anyone to forget to bump.
    """
    h = hashlib.sha256()
    if not SKILLS_DIR or not os.path.isdir(SKILLS_DIR):
        # No bases found means no basis for trusting a cached verdict about them.
        # Returning a unique value here makes every lookup miss, which is the
        # right failure: slow, not wrong.
        return "no-skills-dir"
    for root, dirs, files in os.walk(SKILLS_DIR):
        dirs.sort()
        rel = os.path.relpath(root, SKILLS_DIR).replace(os.sep, "/")
        if not (rel.startswith("sh-kb-") or rel == "."):
            continue
        for f in sorted(files):
            if not f.endswith((".md", ".json")):
                continue
            p = os.path.join(root, f)
            h.update(os.path.relpath(p, SKILLS_DIR).replace(os.sep, "/").encode("utf-8"))
            with open(p, "rb") as fh:
                h.update(fh.read())
    return h.hexdigest()[:16]


def skill_version():
    if not MANIFEST:
        return "unknown"
    try:
        with io.open(MANIFEST, encoding="utf-8") as fh:
            return json.load(fh).get("version", "unknown")
    except Exception:
        return "unknown"


def key_context(model=None):
    return {
        "schema": SCHEMA,
        "kb_version": kb_version(),
        "skill_version": skill_version(),
        "model": model or os.environ.get("SH_REVIEW_MODEL") or "unspecified",
    }


def file_sha(path):
    h = hashlib.sha256()
    try:
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(65536), b""):
                h.update(chunk)
    except (IOError, OSError):
        return None
    return h.hexdigest()[:16]


# --------------------------------------------------------------------------
# commands
# --------------------------------------------------------------------------

def out(obj):
    print(json.dumps(obj, indent=2, sort_keys=True))


def cmd_kb_version(a):
    print(kb_version())
    return 0


def cmd_key(a):
    out(key_context(a.model))
    return 0


def miss(reason, **extra):
    d = {"hit": False, "reason": reason}
    d.update(extra)
    out(d)
    return 0


def cmd_get(a):
    p = entry_path(a.repo, a.pr)
    if not os.path.isfile(p):
        return miss("no cached run for this PR")
    try:
        with io.open(p, encoding="utf-8") as fh:
            entry = json.load(fh)
    except Exception as e:
        return miss("cache entry unreadable (%s)" % e.__class__.__name__)

    want = key_context(a.model)

    if entry.get("schema") != SCHEMA:
        return miss("cache schema changed")
    if entry.get("kb_version") != want["kb_version"]:
        return miss("knowledge bases changed since this run was cached")
    if entry.get("skill_version") != want["skill_version"]:
        return miss("skill version changed since this run was cached")
    # An unspecified model on either side is not a match: a haiku verdict and an
    # opus verdict are not interchangeable, and guessing they are is how a weak
    # verdict gets reused as a strong one.
    if entry.get("model") != want["model"] or want["model"] == "unspecified":
        return miss("model differs or is unspecified",
                    cached_model=entry.get("model"), current_model=want["model"])

    created = entry.get("created_at")
    if created:
        try:
            age = (_dt.datetime.now(_dt.timezone.utc)
                   - _dt.datetime.fromisoformat(created)).days
            if age > a.max_age_days:
                return miss("cached run is %d days old (max %d)" % (age, a.max_age_days))
        except Exception:
            return miss("cache entry has an unparseable timestamp")
    else:
        return miss("cache entry has no timestamp")

    entry["hit"] = True
    out(entry)
    return 0


def cmd_put(a):
    try:
        with io.open(a.file, encoding="utf-8") as fh:
            entry = json.load(fh)
    except Exception as e:
        sys.stderr.write("error: cannot read %s: %s\n" % (a.file, e))
        return 1
    if not isinstance(entry, dict):
        sys.stderr.write("error: entry must be a JSON object\n")
        return 1

    entry.update(key_context(a.model))
    entry["repo"] = a.repo
    entry["pr"] = a.pr
    entry["created_at"] = _dt.datetime.now(_dt.timezone.utc).isoformat()
    entry.pop("hit", None)

    p = entry_path(a.repo, a.pr)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    tmp = p + ".tmp"
    with io.open(tmp, "w", encoding="utf-8", newline="") as fh:
        fh.write(json.dumps(entry, indent=2, sort_keys=True))
    os.replace(tmp, p)
    out({"stored": p, "kb_version": entry["kb_version"],
         "skill_version": entry["skill_version"], "model": entry["model"]})
    return 0


def cmd_changed(a):
    """Which of the given paths differ from the cached run.

    Input file: {"path": "<content sha>", ...} for the tree being reviewed now.
    Output splits them into paths that must be re-analysed and paths whose
    cached findings can be reused verbatim. A path the cache has never seen is
    always 'changed'; never assume an unseen file is clean.
    """
    try:
        with io.open(a.file, encoding="utf-8") as fh:
            current = json.load(fh)
    except Exception as e:
        sys.stderr.write("error: cannot read %s: %s\n" % (a.file, e))
        return 1

    p = entry_path(a.repo, a.pr)
    if not os.path.isfile(p):
        out({"reusable": [], "changed": sorted(current),
             "reason": "no cached run; everything must be analysed"})
        return 0
    with io.open(p, encoding="utf-8") as fh:
        cached = json.load(fh).get("files", {})

    changed, reusable = [], []
    for path, sha in sorted(current.items()):
        (reusable if cached.get(path) == sha and sha is not None else changed).append(path)
    # A path the cache knew about that is now gone still matters: its findings
    # must be dropped, not carried forward as if still present.
    vanished = sorted(set(cached) - set(current))
    out({"reusable": reusable, "changed": changed, "vanished": vanished})
    return 0


def _walk_entries():
    root = cache_root()
    if not os.path.isdir(root):
        return
    for slug in sorted(os.listdir(root)):
        d = os.path.join(root, slug)
        if not os.path.isdir(d):
            continue
        for f in sorted(os.listdir(d)):
            if f.endswith(".json"):
                yield slug, os.path.join(d, f)


def cmd_stat(a):
    entries, total = [], 0
    for slug, p in _walk_entries():
        if a.repo and slug != repo_slug(a.repo):
            continue
        size = os.path.getsize(p)
        total += size
        try:
            with io.open(p, encoding="utf-8") as fh:
                e = json.load(fh)
            entries.append({"repo": e.get("repo", slug), "pr": e.get("pr"),
                            "head_sha": (e.get("head_sha") or "")[:8],
                            "findings": len(e.get("findings", [])),
                            "created_at": e.get("created_at"),
                            "stale_kb": e.get("kb_version") != kb_version(),
                            "bytes": size})
        except Exception:
            entries.append({"repo": slug, "unreadable": os.path.basename(p), "bytes": size})
    out({"root": cache_root(), "entries": len(entries),
         "bytes": total, "current_kb_version": kb_version(), "items": entries})
    return 0


def cmd_prune(a):
    now = _dt.datetime.now(_dt.timezone.utc)
    removed = []
    for _slug, p in _walk_entries():
        drop = False
        try:
            with io.open(p, encoding="utf-8") as fh:
                e = json.load(fh)
            created = e.get("created_at")
            if not created:
                drop = True
            else:
                if (now - _dt.datetime.fromisoformat(created)).days > a.max_age_days:
                    drop = True
            if e.get("schema") != SCHEMA:
                drop = True
        except Exception:
            drop = True  # unreadable entries are never useful
        if drop:
            os.remove(p)
            removed.append(p)
    out({"removed": len(removed), "paths": removed})
    return 0


def cmd_clear(a):
    root = cache_root()
    target = os.path.join(root, repo_slug(a.repo)) if a.repo else root
    if os.path.isdir(target):
        shutil.rmtree(target)
        out({"cleared": target})
    else:
        out({"cleared": None, "reason": "nothing at %s" % target})
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd")

    def add_repo_pr(p):
        p.add_argument("--repo", required=True, help="owner/repo")
        p.add_argument("--pr", required=True, help="PR number")

    p = sub.add_parser("kb-version"); p.set_defaults(fn=cmd_kb_version)

    p = sub.add_parser("key"); p.add_argument("--model", default=None)
    p.set_defaults(fn=cmd_key)

    p = sub.add_parser("get"); add_repo_pr(p)
    p.add_argument("--model", default=None)
    p.add_argument("--max-age-days", type=int, default=DEFAULT_TTL_DAYS)
    p.set_defaults(fn=cmd_get)

    p = sub.add_parser("put"); add_repo_pr(p)
    p.add_argument("--file", required=True, help="JSON run state to store")
    p.add_argument("--model", default=None)
    p.set_defaults(fn=cmd_put)

    p = sub.add_parser("changed"); add_repo_pr(p)
    p.add_argument("--file", required=True, help='JSON {"path": "<sha>"} of the current tree')
    p.set_defaults(fn=cmd_changed)

    p = sub.add_parser("stat"); p.add_argument("--repo", default=None)
    p.set_defaults(fn=cmd_stat)

    p = sub.add_parser("prune")
    p.add_argument("--max-age-days", type=int, default=DEFAULT_TTL_DAYS)
    p.set_defaults(fn=cmd_prune)

    p = sub.add_parser("clear"); p.add_argument("--repo", default=None)
    p.set_defaults(fn=cmd_clear)

    a = ap.parse_args(argv)
    if not getattr(a, "fn", None):
        ap.print_help()
        return 2
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
