# Tooling setup - install missing tools (Stage 0)

The pipeline works with zero external tools (it degrades to native search + manifest parsing), but each
tool improves fidelity. Stage 0 **installs the missing ones automatically** using whatever package manager
is already on the machine. Rules that apply to every install below:

- **Announce before installing.** State which tool and which command. These change the system.
- **Prefer no-elevation, user-scope installs.** Use non-interactive flags (`-y`/`--yes`/`--accept-*`).
  Only use a `sudo`/admin method if the platform requires it AND a package manager is already configured for
  it; never launch an interactive elevation prompt in an automated run - if elevation is required and not
  available, skip and mark the tool unavailable.
- **Never block the run.** If an install fails or no installer is available, record the tool `false` in
  `capabilities.json` with a short `notes` reason and continue. The pipeline handles absence.
- **Only install what adds a missing capability group** (don't install three CVE scanners):
  - **Codebase map** → `graft` (see graft-guide.md; installed + wired separately in Stage 0 step 4).
  - **SBOM** → `syft`.
  - **CVEs** → the first of `grype` / `trivy` / `osv-scanner` you can install (prefer **grype** - it consumes
    syft's SBOM directly). Stop once one is working.
  - **Long-term memory** → `indra` (skipped with `memory:off`).
  - **PDF/doc reports** → one PDF engine: prefer `wkhtmltopdf`; `pandoc` also enables `.docx`. Headless
    Chrome (often already present) is a third fallback the reporter can use, so a PDF engine is optional.
- **Re-check after installing** (`<tool> --version`) and set the capability accordingly. A freshly installed
  binary may need a new shell / PATH entry - if the version check fails right after install, note it and
  treat as unavailable for this run.

## Detect the available package manager first

Probe once and pick the first that exists; reuse it for every tool. `<tool> --version` / `-v` presence
checks: any non-zero exit or "not found" means absent.

| Platform | Preferred managers (in order) |
|---|---|
| Windows | `winget` → `choco` → `scoop` |
| macOS | `brew` |
| Linux | `brew` (if present) → distro pkg mgr (`apt-get`/`dnf`/`apk`, may need sudo) → vendor install script |
| Any | `npm` (graft), `pipx`/`pip` (python tools), `go install` (Go tools) - when those runtimes exist |

## Per-tool install matrix

Run the first command whose manager/runtime is available; fall through on failure.

### graft  (codebase map - see graft-guide.md for the full flow)
```
npm install -g @nanonets/graft
```

### syft  (SBOM)
```
winget install --id Anchore.Syft -e --accept-source-agreements --accept-package-agreements   # Windows
choco install syft -y                                                                          # Windows
scoop install syft                                                                             # Windows
brew install syft                                                                              # macOS/Linux
curl -sSfL https://raw.githubusercontent.com/anchore/syft/main/install.sh | sh -s -- -b "$HOME/.local/bin"   # Linux/macOS, user-scope
```

### grype  (CVEs - preferred; pairs with syft SBOM)
```
winget install --id Anchore.Grype -e --accept-source-agreements --accept-package-agreements   # Windows
choco install grype -y                                                                         # Windows
scoop install grype                                                                            # Windows
brew install grype                                                                             # macOS/Linux
curl -sSfL https://raw.githubusercontent.com/anchore/grype/main/install.sh | sh -s -- -b "$HOME/.local/bin"   # Linux/macOS
```

### trivy  (CVEs - fallback if grype unavailable)
```
choco install trivy -y                       # Windows
scoop install trivy                          # Windows
brew install trivy                           # macOS/Linux
curl -sfL https://raw.githubusercontent.com/aquasecurity/trivy/main/contrib/install.sh | sh -s -- -b "$HOME/.local/bin"   # Linux/macOS
```

### osv-scanner  (CVEs - fallback)
```
scoop install osv-scanner                    # Windows
brew install osv-scanner                     # macOS/Linux
go install github.com/google/osv-scanner/cmd/osv-scanner@latest   # any, if Go is installed (binary lands in `go env GOPATH`/bin)
```

### pandoc  (PDF + .docx reports)
```
winget install --id JGM.Pandoc -e --accept-source-agreements --accept-package-agreements       # Windows
choco install pandoc -y                                                                        # Windows
scoop install pandoc                                                                           # Windows
brew install pandoc                                                                            # macOS/Linux
sudo apt-get install -y pandoc                                                                 # Debian/Ubuntu (only if apt + non-interactive sudo already work)
```

### wkhtmltopdf  (HTML→PDF, preferred PDF engine)
```
winget install --id wkhtmltopdf.wkhtmltopdf -e --accept-source-agreements --accept-package-agreements   # Windows
choco install wkhtmltopdf -y                                                                   # Windows
scoop install wkhtmltopdf                                                                      # Windows
brew install --cask wkhtmltopdf                                                                # macOS
sudo apt-get install -y wkhtmltopdf                                                            # Debian/Ubuntu (only if apt + non-interactive sudo already work)
```

### indra  (long-term memory via `indra mem` - optional, see the sh-memory skill)

A single static binary. Prefer a prebuilt release binary (no toolchain needed), else build from source
with Cargo:

```
# Prebuilt: download indra-<target> from the latest `indra-v*` release of
# https://github.com/dmdhrumilmistry/security-harness/releases into a directory on PATH
# (for example ~/.local/bin, or %LOCALAPPDATA%\Programs\indra on Windows).
cargo install --locked --git https://github.com/dmdhrumilmistry/security-harness indra   # any OS with Rust
```

If neither works, set `indra:false`: the pipeline runs without long-term memory.

## After installs

Write the final capability matrix to `capabilities.json` (see state-files.md), including which tools were
installed this run (`notes`). Report the matrix to the user in the Stage 0 announcement so they know what
was added to their machine and what the run will use vs. fall back on.
