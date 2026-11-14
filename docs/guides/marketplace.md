# Marketplace: plugins, skills and harnesses

The console's **Marketplace** page browses Claude Code plugin marketplaces and skill
repositories, and installs what they hold for Claude Code, Codex or an indra harness. Three are
built in; add any other.

| Built in | Source | What it holds |
|---|---|---|
| `indra` | `dmdhrumilmistry/indra` | the indra security-review plugin |
| `anthropic-skills` | `anthropics/skills` | Anthropic's document and example skills |
| `claude-plugins-official` | `anthropics/claude-plugins-official` | the official Claude Code plugin directory |

## Sources

A marketplace is a git repository (or a folder) with a `.claude-plugin/marketplace.json`, the
format Claude Code uses. A repository without one is read as a set of skills: every folder with a
`SKILL.md` becomes an entry. Add one with **Add a marketplace** on the page, in
**Settings > Marketplaces**, or in `indra.toml`:

```toml
[[marketplace]]
name   = "team"
source = "my-org/agent-plugins"     # owner/repo on GitHub, an https git URL, or a local folder
ref    = "v2"                       # optional branch, tag or commit
```

A configured marketplace with a built-in's name replaces it. Nothing is downloaded until you
press **Fetch** or **Refresh**; indra then makes a shallow clone with the system `git` into its
data directory. Nothing from a marketplace is ever run by indra.

## Browse and install

Pick a marketplace, search or filter by category, and open a plugin. **Look inside** lists its
skills, agents and commands (fetching it first when it lives in another repository, as many in
the official directory do: `github`, `url` and `git-subdir` sources, pinned commits included).
Untick what you do not want, choose where it goes, and **Install**:

| For | Where the files go |
|---|---|
| Claude Code, for me | `~/.claude/skills/<name>/`, `~/.claude/agents/`, `~/.claude/commands/` |
| Claude Code, in a project | `<target>/.claude/...` |
| Codex, for me | `~/.agents/skills/<name>/` (skills only) |
| Codex, in a project | `<target>/.agents/skills/<name>/` (skills only) |
| The current harness | its own folders, from its `skills`, `agents` and `prompts` patterns |
| A new harness | a `[[harness]]` that reads the plugin where it was fetched, so you can view its agents and skills and launch them |

An install never overwrites a file it did not write unless you tick **Replace**. A plugin's hooks,
MCP servers and LSP servers only take effect through Claude Code's own plugin system, so for
those (and for plugins whose source is a command, npm or an archive) use **Install it as a Claude
Code plugin**: indra runs `claude plugin marketplace add <source>` and then
`claude plugin install <plugin>@<marketplace>` with the scope you pick, on this machine. Claude
Code then manages it (`/plugin`).

## Installed

The **Installed** tab lists every install indra made: the plugin, where, which items, the
version and commit. **Update** fetches the marketplace (and the plugin) again and installs the
same items in the same place; **Remove** deletes exactly the files that install wrote and
nothing else. The record lives in `installed.json` in the data directory.

## Trust

Skills and agents tell agents what to do, and a skill's scripts run with your permissions when an
agent uses them. Install only from sources you trust, read what **Look inside** shows, and pin a
`ref` for marketplaces you depend on.

## API

| Call | What it does |
|---|---|
| `GET /api/market` | marketplaces, their fetch state and plugin counts, and the installs |
| `POST /api/market/refresh` `{m}` | fetch a marketplace |
| `GET /api/market/plugins?m=` | its plugins, marked where installed |
| `POST /api/market/inspect` `{m, plugin}` | a plugin's skills, agents, commands, and what only Claude Code can install |
| `POST /api/market/install` `{m, plugin, dest, h, target, items, overwrite}` | copy items; `dest` is `claude-user`, `claude-project`, `codex-user`, `codex-project` or `harness` |
| `POST /api/market/native` `{m, plugin, scope, h, target}` | install with Claude Code |
| `POST /api/market/installed/{id}/update`, `DELETE /api/market/installed/{id}` | update or remove an install |
