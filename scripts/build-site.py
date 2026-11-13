#!/usr/bin/env python3
"""Build the project website into a folder ready for GitHub Pages.

    python3 scripts/build-site.py            # writes _site/
    python3 scripts/build-site.py --out DIR

The landing page is hand-written (site/). The documentation pages are
rendered from the Markdown that already lives in the repository (README.md,
tools/indra/README.md, docs/), so the docs have one source of truth and the
website never has to send readers to GitHub to read them.

Only the standard library is used. The Markdown renderer covers what this
repository's docs use: headings, paragraphs, lists, tables, fenced code,
block quotes, rules, raw HTML blocks, links, images, emphasis and inline
code. Links between documents are rewritten to the matching docs page and
anchor; links to anything else in the repository go to that file on
GitHub. A link to a heading that does not exist fails the build.
"""

import argparse
import html
import os
import re
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GH = "https://github.com/dmdhrumilmistry/indra"
SITE_URL = "https://dmdhrumilmistry.github.io/indra/"

# (group, slug, title, description, [(source, sections)])
# sections: None = the whole file (its H1 is dropped); otherwise a list of H2
# titles to take from the file, where "" means the text before the first H2.
PAGES = [
    ("Start", "index", "Getting started",
     "Install indra for Claude Code, Gemini CLI, Codex or opencode, and run your first review.",
     [("README.md", ["Install", "Usage"])]),
    ("Start", "how-it-works", "How it works",
     "The review pipeline stage by stage, what the plugin contains, and how to add a vulnerability class.",
     [("README.md", ["How it works", "What's inside", "Extending"])]),
    ("Reviews", "pr-review", "Pull request review",
     "Review one pull request, post inline comments, and re-review incrementally.",
     [("README.md", ["Pull request review"])]),
    ("indra CLI", "cli", "The indra CLI",
     "One small binary: graph memory and an agent web console. Install and build it.",
     [("tools/indra/README.md", ["", "Install", "Development"])]),
    ("indra CLI", "memory", "Memory",
     "indra mem: Markdown notes with typed links, graph-aware recall, and graph views.",
     [("tools/indra/README.md", ["Memory: `indra mem`"])]),
    ("indra CLI", "console", "Agent console",
     "indra serve: view and edit agents, watch runs, launch agents and follow them live.",
     [("tools/indra/README.md", ["Console: `indra serve`"])]),
    ("indra CLI", "orchestration", "Nodes and orchestration",
     "Run agents on this machine and over SSH, and chain them into workflows.",
     [("tools/indra/README.md", ["Nodes and orchestration"])]),
    ("Guides", "processes", "Handling agent processes",
     "Launch, watch, stop and clean up after agents; logs, capacity, permissions, troubleshooting.",
     [("docs/guides/processes.md", None)]),
    ("Guides", "pivoting", "Pivoting agents",
     "Redirect an agent: continue its session with a new instruction, or start fresh with a brief.",
     [("docs/guides/pivoting.md", None)]),
    ("Guides", "knowledge-base", "Knowledge base and integrations",
     "Import Obsidian, Notion and GitHub wikis; publish findings to Jira, Confluence, GitHub and Notion.",
     [("docs/guides/knowledge-base.md", None)]),
    ("Guides", "board", "The board and trackers",
     "Work items run by agents, synced with Jira, GitHub Issues and ClickUp.",
     [("docs/guides/board.md", None)]),
    ("Project", "architecture", "Architecture",
     "How indra controls agents across machines: profiles, secrets, transport, the board and graph workflows.",
     [("docs/architecture.md", None)]),
    ("Project", "kb-updates", "Knowledge-base updates",
     "The scheduled workflows that keep the vulnerability knowledge bases current.",
     [("README.md", ["Automated knowledge-base updates"])]),
    ("Project", "distribution", "Distribution",
     "How indra is packaged for each agent, and the public listings.",
     [("docs/DISTRIBUTION.md", None)]),
]

# Headings that are not on any page but are linked to: where they live now.
ALIASES = {
    ("README.md", "indra-memory-and-agent-console"): ("cli", ""),
    ("README.md", "long-term-memory-indra-mem"): ("memory", ""),
    ("README.md", "agent-console-indra-serve"): ("console", ""),
    ("README.md", "agents-on-many-machines-indra-nodes-indra-run"): ("orchestration", ""),
}

HTML_BLOCK = re.compile(r"^\s*<(p|div|table|img|details|h[1-6]|picture|figure|ul|ol|br|hr)\b", re.I)
INLINE_TAGS = r"b|i|em|strong|sub|sup|br|kbd|code|s|u"


# ---------------------------------------------------------------- sources

def fence_of(line):
    m = re.match(r"^\s*(```+|~~~+)", line)
    return m.group(1) if m else None


def split_h2(lines):
    """[(title or "", lines)] split at H2 headings outside code fences."""
    out, cur, title, fence = [], [], "", None
    for ln in lines:
        f = fence_of(ln)
        if fence:
            if f and f[0] == fence[0] and len(f) >= len(fence) and ln.strip() == f:
                fence = None
        elif f:
            fence = f
        elif ln.startswith("## "):
            out.append((title, cur))
            title, cur = ln[3:].strip(), []
            continue
        cur.append(ln)
    out.append((title, cur))
    return out


def drop_leading_html(lines):
    """Skip a README's decorative header: HTML blocks and an H1 before the text."""
    i = 0
    while i < len(lines):
        s = lines[i].strip()
        if not s or s.startswith("<") or s.startswith("# "):
            # an HTML block runs to the next blank line
            if s.startswith("<"):
                while i < len(lines) and lines[i].strip():
                    i += 1
                continue
            i += 1
            continue
        break
    return lines[i:]


def page_markdown(sources):
    """The Markdown for one page, as [(source, lines)] chunks."""
    chunks = []
    for src, sections in sources:
        lines = (REPO / src).read_text(encoding="utf-8").splitlines()
        if sections is None:
            if lines and lines[0].startswith("# "):
                lines = lines[1:]
            chunks.append((src, lines, None))
            continue
        parts = dict(split_h2(lines))
        for name in sections:
            if name not in parts:
                raise SystemExit("build-site: %s has no section %r" % (src, name))
            body = parts[name]
            if name == "":
                chunks.append((src, drop_leading_html(body), None))
            else:
                chunks.append((src, body, name))
    return chunks


# ---------------------------------------------------------------- slugs

def slugify(text):
    """GitHub's heading anchors."""
    t = re.sub(r"<[^>]+>", "", text)
    t = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", t)
    t = t.strip().lower()
    t = re.sub(r"[^\w\- ]", "", t)
    return t.replace(" ", "-")


def heading_text(raw):
    return re.sub(r"\s+#+\s*$", "", raw).strip()


# ---------------------------------------------------------------- links

class Links:
    def __init__(self):
        self.anchors = {}      # (source, id) -> (slug, id)
        self.first_page = {}   # source -> slug
        self.errors = []

    def register(self, src, hid, slug, page_id):
        self.anchors.setdefault((src, hid), (slug, page_id))
        self.first_page.setdefault(src, slug)

    def resolve(self, href, src, here):
        """Rewrite a link found in `src`, rendered on page `here`."""
        if not href or href.startswith(("http://", "https://", "mailto:", "data:")):
            return href
        if href.startswith("#"):
            return self.page_link(src, href[1:], here, href)
        path, _, frag = href.partition("#")
        target = os.path.normpath(os.path.join(os.path.dirname(src), path)).replace("\\", "/")
        if target.startswith("../"):
            return href
        if target in self.first_page:
            return self.page_link(target, frag, here, href)
        if target == "assets" or target.startswith("assets/"):
            return "../" + target
        kind = "tree" if (REPO / target).is_dir() else "blob"
        return "%s/%s/main/%s%s" % (GH, kind, target, ("#" + frag) if frag else "")

    def page_link(self, src, frag, here, original):
        if not frag:
            slug = self.first_page[src]
            return "%s.html" % slug if slug != here else "#"
        hit = self.anchors.get((src, frag)) or ALIASES.get((src, frag))
        if not hit:
            self.errors.append("%s: link %r on page %s points to a heading that does not exist" % (src, original, here))
            return original
        slug, pid = hit
        page = "" if slug == here else "%s.html" % slug
        return page + ("#" + pid if pid else "") or "#"


# ---------------------------------------------------------------- markdown

def esc(s):
    return html.escape(s, quote=False)


def attr(s):
    return html.escape(s, quote=True)


class Renderer:
    def __init__(self, links, src, here):
        self.links, self.src, self.here = links, src, here
        self.ids = {}
        self.toc = []

    # -- inline
    def inline(self, text):
        codes = []

        def keep(m):
            codes.append("<code>%s</code>" % esc(m.group(2).strip() if m.group(2).strip() else m.group(2)))
            return "\x00%d\x00" % (len(codes) - 1)

        text = re.sub(r"(`+)(.+?)\1", keep, text)

        def autolink(m):
            codes.append('<a href="%s" rel="noopener">%s</a>' % (attr(m.group(1)), esc(m.group(1))))
            return "\x00%d\x00" % (len(codes) - 1)

        text = re.sub(r"<((?:https?://|mailto:)[^>\s]+)>", autolink, text)
        text = esc(text)
        text = re.sub(r"&lt;(/?(?:%s))(\s*/?)&gt;" % INLINE_TAGS, r"<\1\2>", text, flags=re.I)

        def img(m):
            src = self.links.resolve(html.unescape(m.group(2)), self.src, self.here)
            return '<img src="%s" alt="%s" loading="lazy">' % (attr(src), attr(html.unescape(m.group(1))))

        def link(m):
            href = self.links.resolve(html.unescape(m.group(2)), self.src, self.here)
            ext = ' rel="noopener"' if href.startswith("http") else ""
            return '<a href="%s"%s>%s</a>' % (attr(href), ext, m.group(1))

        text = re.sub(r"!\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+&quot;[^)]*&quot;)?\s*\)", img, text)
        text = re.sub(r"\[((?:[^\[\]]|\[[^\]]*\])+)\]\(\s*([^)\s]+)\s*\)", link, text)
        text = re.sub(r"(?<![\w\"=/])(https?://[^\s<)]+[^\s<).,;:])", r'<a href="\1" rel="noopener">\1</a>', text)
        text = re.sub(r"\*\*(?=\S)(.+?)(?<=\S)\*\*", r"<strong>\1</strong>", text, flags=re.S)
        text = re.sub(r"(?<![\w*])\*(?=\S)([^*]+?)(?<=\S)\*(?![\w*])", r"<em>\1</em>", text)
        text = re.sub(r"(?<![\w_])_(?=\S)([^_]+?)(?<=\S)_(?![\w_])", r"<em>\1</em>", text)
        return re.sub(r"\x00(\d+)\x00", lambda m: codes[int(m.group(1))], text)

    def raw_html(self, block):
        def fix(m):
            return '%s="%s"' % (m.group(1), attr(self.links.resolve(html.unescape(m.group(2)), self.src, self.here)))
        return re.sub(r'\b(href|src)="([^"]*)"', fix, block)

    def heading(self, level, raw):
        text = heading_text(raw)
        base = slugify(text)
        n = self.ids.get(base, 0)
        self.ids[base] = n + 1
        hid = base if n == 0 else "%s-%d" % (base, n)
        if level in (2, 3):
            self.toc.append((level, hid, self.inline(text)))
        return '<h%d id="%s">%s</h%d>' % (level, hid, self.inline(text), level)

    # -- blocks
    def blocks(self, lines):
        out, i, n = [], 0, len(lines)
        while i < n:
            ln = lines[i]
            s = ln.strip()
            if not s:
                i += 1
                continue
            f = fence_of(ln)
            if f:
                lang = s[len(f):].strip().split()[0] if s[len(f):].strip() else ""
                indent = len(ln) - len(ln.lstrip())
                body, i = [], i + 1
                while i < n and not (lines[i].strip().startswith(f[0] * len(f)) and lines[i].strip() == lines[i].strip()[0] * len(lines[i].strip()) and len(lines[i].strip()) >= len(f)):
                    body.append(lines[i][indent:] if lines[i][:indent].strip() == "" else lines[i])
                    i += 1
                i += 1
                cls = ' class="language-%s"' % attr(lang) if lang else ""
                out.append("<pre><code%s>%s</code></pre>" % (cls, esc("\n".join(body))))
                continue
            m = re.match(r"^(#{1,6})\s+(.*)$", s)
            if m:
                out.append(self.heading(len(m.group(1)), m.group(2)))
                i += 1
                continue
            if re.match(r"^(-{3,}|\*{3,}|_{3,})$", s):
                out.append("<hr>")
                i += 1
                continue
            if HTML_BLOCK.match(ln):
                block = []
                while i < n and lines[i].strip():
                    block.append(lines[i])
                    i += 1
                out.append(self.raw_html("\n".join(block)))
                continue
            if s.startswith("|") and i + 1 < n and re.match(r"^\s*\|?\s*:?-{2,}", lines[i + 1]):
                rows = []
                while i < n and lines[i].strip().startswith("|"):
                    rows.append(lines[i].strip())
                    i += 1
                out.append(self.table(rows))
                continue
            if s.startswith(">"):
                quote = []
                while i < n and lines[i].strip().startswith(">"):
                    quote.append(re.sub(r"^\s*>\s?", "", lines[i]))
                    i += 1
                out.append("<blockquote>%s</blockquote>" % self.blocks(quote))
                continue
            if LIST_ITEM.match(ln):
                html_, i = self.list_(lines, i)
                out.append(html_)
                continue
            para = []
            while i < n and lines[i].strip() and not self.starts_block(lines, i):
                para.append(lines[i])
                i += 1
            text = "\n".join(p.rstrip() if not p.endswith("  ") else p.rstrip() + "<br>" for p in para)
            out.append("<p>%s</p>" % self.inline(text).replace("&lt;br&gt;", "<br>"))
        return "\n".join(out)

    def starts_block(self, lines, i):
        ln = lines[i]
        s = ln.strip()
        return bool(fence_of(ln) or re.match(r"^#{1,6}\s", s) or s.startswith(">") or HTML_BLOCK.match(ln)
                    or LIST_ITEM.match(ln) or (s.startswith("|") and i + 1 < len(lines) and re.match(r"^\s*\|?\s*:?-{2,}", lines[i + 1])))

    def list_(self, lines, i):
        m = LIST_ITEM.match(lines[i])
        indent = len(m.group(1))
        ordered = m.group(2)[0].isdigit()
        first = int(m.group(2)[:-1]) if ordered else 1
        items, n = [], len(lines)
        while i < n:
            m = LIST_ITEM.match(lines[i])
            if not m or len(m.group(1)) != indent or m.group(2)[0].isdigit() != ordered:
                break
            content_indent = len(m.group(0))
            body = [lines[i][content_indent:]]
            i += 1
            while i < n:
                ln = lines[i]
                if not ln.strip():
                    # a blank line ends the item unless the next line is indented into it
                    if i + 1 < n and lines[i + 1].strip() and len(lines[i + 1]) - len(lines[i + 1].lstrip()) > indent:
                        body.append("")
                        i += 1
                        continue
                    break
                mi = LIST_ITEM.match(ln)
                if mi and len(mi.group(1)) <= indent:
                    break
                lead = len(ln) - len(ln.lstrip())
                if lead > indent:
                    body.append(ln[min(lead, content_indent):])
                elif fence_of(ln) or re.match(r"^#{1,6}\s", ln) or HTML_BLOCK.match(ln):
                    break
                else:
                    body.append(ln.strip())  # a lazy continuation line
                i += 1
            items.append(body)
            # tolerate a single blank line between items
            if i < n and not lines[i].strip() and i + 1 < n:
                mn = LIST_ITEM.match(lines[i + 1])
                if mn and len(mn.group(1)) == indent:
                    i += 1
        tag = "ol" if ordered else "ul"
        start = ' start="%d"' % first if ordered and first != 1 else ""
        parts = []
        for body in items:
            if any(LIST_ITEM.match(b) or fence_of(b) or not b.strip() for b in body[1:]):
                inner = self.blocks(body)
                if inner.startswith("<p>") and inner.count("<p>") == 1:
                    inner = inner[3:].replace("</p>", "", 1)
            else:
                inner = self.inline("\n".join(body))
            parts.append("<li>%s</li>" % inner)
        return "<%s%s>%s</%s>" % (tag, start, "".join(parts), tag), i

    def table(self, rows):
        def cells(row):
            row = row.strip().strip("|")
            out, cur, code = [], "", False
            j = 0
            while j < len(row):
                c = row[j]
                if c == "\\" and j + 1 < len(row) and row[j + 1] == "|":
                    cur += "|"
                    j += 2
                    continue
                if c == "`":
                    code = not code
                if c == "|" and not code:
                    out.append(cur.strip())
                    cur = ""
                else:
                    cur += c
                j += 1
            out.append(cur.strip())
            return out

        head = cells(rows[0])
        aligns = []
        for c in cells(rows[1]):
            aligns.append("right" if c.endswith(":") and not c.startswith(":") else "center" if c.startswith(":") and c.endswith(":") else "")
        def tr(cs, tag):
            res = []
            for k, c in enumerate(cs):
                al = aligns[k] if k < len(aligns) and aligns[k] else ""
                st = ' style="text-align:%s"' % al if al else ""
                res.append("<%s%s>%s</%s>" % (tag, st, self.inline(c), tag))
            return "<tr>%s</tr>" % "".join(res)
        body = "".join(tr(cells(r), "td") for r in rows[2:])
        return '<div class="table"><table><thead>%s</thead><tbody>%s</tbody></table></div>' % (tr(head, "th"), body)


LIST_ITEM = re.compile(r"^(\s*)([-*+]|\d+[.)])\s+")


# ---------------------------------------------------------------- pages

def nav_html(active):
    groups, out = [], []
    for g, slug, title, _, _ in PAGES:
        if not groups or groups[-1][0] != g:
            groups.append((g, []))
        groups[-1][1].append((slug, title))
    for g, items in groups:
        out.append("<p>%s</p><ul>" % esc(g))
        for slug, title in items:
            cur = ' aria-current="page"' if slug == active else ""
            out.append('<li><a href="%s.html"%s>%s</a></li>' % (slug, cur, esc(title)))
        out.append("</ul>")
    return "\n".join(out)


TEMPLATE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} - indra docs</title>
<meta name="description" content="{desc}">
<meta property="og:title" content="{title} - indra docs">
<meta property="og:description" content="{desc}">
<meta property="og:image" content="{site}assets/social.png">
<meta property="og:type" content="article">
<link rel="canonical" href="{site}docs/{slug}.html">
<link rel="icon" href="../assets/icon.svg" type="image/svg+xml">
<link rel="stylesheet" href="../style.css">
</head>
<body class="docs-page">
<header class="nav">
  <div class="wrap nav-in">
    <a class="brand" href="../index.html"><img src="../assets/logo.svg" alt="" width="30" height="30"><span>indra</span></a>
    <nav aria-label="Site">
      <a href="../index.html#pipeline">Pipeline</a>
      <a href="../index.html#memory">Memory</a>
      <a href="../index.html#console">Console</a>
      <a href="../index.html#install">Install</a>
      <a class="gh" href="index.html" aria-current="page">Docs</a>
    </nav>
  </div>
</header>

<div class="wrap docs">
  <details class="side" open>
    <summary>Menu <span>{title}</span></summary>
    <nav aria-label="Documentation">
{nav}
    </nav>
  </details>
  <main class="doc" id="top">
    <p class="eyebrow">{group}</p>
    <h1>{title}</h1>
    <p class="sub">{desc}</p>
{body}
    <nav class="pager" aria-label="Previous and next page">{pager}</nav>
  </main>
  <aside class="toc" aria-label="On this page">
{toc}
  </aside>
</div>

<footer class="foot">
  <div class="wrap foot-in">
    <span class="brand"><img src="../assets/logo.svg" alt="" width="22" height="22"><span>indra</span></span>
    <span>MIT licensed &middot; static analysis of code you are authorized to review</span>
    <a href="{gh}" rel="noopener">Source code</a>
  </div>
</footer>
<script src="../main.js"></script>
</body>
</html>
"""


def build(out):
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(REPO / "site", out)
    shutil.copytree(REPO / "assets", out / "assets")
    (out / ".nojekyll").write_text("")
    (out / "docs").mkdir()

    links = Links()
    rendered = []
    # Pass 1: every heading's id, so links can point across pages.
    for _, slug, _, _, sources in PAGES:
        for src, _ in sources:
            links.first_page.setdefault(src, slug)
    for _, slug, title, _, sources in PAGES:
        chunks = page_markdown(sources)
        ids = {}

        def take(base):
            k = ids.get(base, 0)
            ids[base] = k + 1
            return base if k == 0 else "%s-%d" % (base, k)

        for src, lines, section in chunks:
            if section is not None:
                # A section that is the page itself (the only one, or named
                # like the page) is not repeated as a heading.
                hid = slugify(section)
                top = len(chunks) == 1 or section == title
                links.register(src, hid, slug, "" if top else take(hid))
            fence = None
            for ln in lines:
                f = fence_of(ln)
                if fence:
                    if f and ln.strip() == f:
                        fence = None
                    continue
                if f:
                    fence = f
                    continue
                m = re.match(r"^(#{1,6})\s+(.*)$", ln.strip())
                if m:
                    base = slugify(heading_text(m.group(2)))
                    links.register(src, base, slug, take(base))

    # Pass 2: render.
    for idx, (group, slug, title, desc, sources) in enumerate(PAGES):
        chunks = page_markdown(sources)
        r = None
        body = []
        for src, lines, section in chunks:
            if r is None:
                r = Renderer(links, src, slug)
            r.src = src
            if section is not None and len(chunks) > 1 and section != title:
                body.append(r.heading(2, section))
            body.append(r.blocks(lines))
        toc = ""
        if r.toc:
            toc = "<p>On this page</p><ul>%s</ul>" % "".join(
                '<li class="l%d"><a href="#%s">%s</a></li>' % (lvl, hid, re.sub(r"<a [^>]*>|</a>", "", txt))
                for lvl, hid, txt in r.toc)
        prev_ = PAGES[idx - 1] if idx > 0 else None
        next_ = PAGES[idx + 1] if idx + 1 < len(PAGES) else None
        pager = (('<a class="prev" href="%s.html"><span>Previous</span>%s</a>' % (prev_[1], esc(prev_[2]))) if prev_ else "<span></span>") + \
                (('<a class="next" href="%s.html"><span>Next</span>%s</a>' % (next_[1], esc(next_[2]))) if next_ else "")
        page = TEMPLATE.format(
            title=esc(title), desc=attr(desc), site=SITE_URL, slug=slug, gh=GH,
            nav=nav_html(slug), group=esc(group), body="\n".join(body), pager=pager, toc=toc,
        )
        (out / "docs" / ("%s.html" % slug)).write_text(page, encoding="utf-8")
        rendered.append(slug)

    if links.errors:
        for e in links.errors:
            print("build-site: " + e, file=sys.stderr)
        raise SystemExit(1)
    return rendered


def check_site_links(out):
    """Every relative href/src in the built site must exist."""
    bad = []
    for page in out.rglob("*.html"):
        text = page.read_text(encoding="utf-8")
        for m in re.finditer(r'\b(?:href|src)="([^"#:]+)(?:#([^"]*))?"', text):
            target = (page.parent / html.unescape(m.group(1))).resolve()
            if not target.exists():
                bad.append("%s -> %s" % (page.relative_to(out), m.group(1)))
                continue
            frag = m.group(2)
            if frag and target.suffix == ".html":
                if not re.search(r'\bid="%s"' % re.escape(frag), target.read_text(encoding="utf-8")):
                    bad.append("%s -> %s#%s (no such anchor)" % (page.relative_to(out), m.group(1), frag))
        for m in re.finditer(r'\bhref="#([^"]+)"', text):
            if not re.search(r'\bid="%s"' % re.escape(m.group(1)), text):
                bad.append("%s -> #%s (no such anchor)" % (page.relative_to(out), m.group(1)))
    return bad


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", default=str(REPO / "_site"))
    a = ap.parse_args()
    out = Path(a.out).resolve()
    pages = build(out)
    bad = check_site_links(out)
    if bad:
        print("build-site: broken links:\n  " + "\n  ".join(bad), file=sys.stderr)
        return 1
    print("built %s: landing page + %d docs pages" % (out, len(pages)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
