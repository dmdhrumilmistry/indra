# Install indra on Windows: one file, nothing else needed.
#
#   irm https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.ps1 | iex
#
# It downloads the Windows build, checks its SHA-256, puts it in
# %LOCALAPPDATA%\Programs\indra (or $env:INDRA_INSTALL_DIR), adds that folder
# to your PATH, and tells you the one command to run next. Run it again to update.
#
# Settings (all optional): $env:INDRA_VERSION = "0.7.0", $env:INDRA_INSTALL_DIR,
# $env:INDRA_NO_MODIFY_PATH = "1", $env:INDRA_RELEASES_URL, $env:INDRA_DOWNLOAD_URL,
# $env:INDRA_NO_COMPLETIONS = "1" (no Tab completion), $env:INDRA_AUTO_UPDATE = "1"
# (keep indra up to date by itself).

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = "dmdhrumilmistry/indra"
$api = if ($env:INDRA_RELEASES_URL) { $env:INDRA_RELEASES_URL } else { "https://api.github.com/repos/$repo/releases" }
$dir = if ($env:INDRA_INSTALL_DIR) { $env:INDRA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\indra" }
$headers = @{ "User-Agent" = "indra-installer" }

if (-not [Environment]::Is64BitOperatingSystem) { throw "indra needs 64-bit Windows." }
$asset = "indra-x86_64-pc-windows-msvc.exe"

# The release asked for, else the newest (tags v0.7.0, or indra-v0.6.0 and
# older; GitHub lists the newest first).
$releases = Invoke-RestMethod -Uri "$($api)?per_page=50" -Headers $headers
$tags = @($releases | Where-Object { -not $_.draft -and -not $_.prerelease -and $_.tag_name -match '^(indra-)?v[0-9]' } | ForEach-Object { $_.tag_name })
if ($env:INDRA_VERSION) {
  $want = $env:INDRA_VERSION -replace '^indra-v', '' -replace '^v', ''
  $tag = $tags | Where-Object { $_ -eq "v$want" -or $_ -eq "indra-v$want" } | Select-Object -First 1
} else {
  $tag = $tags | Select-Object -First 1
}
if (-not $tag) { throw "No indra release $($env:INDRA_VERSION) found." }

$base = if ($env:INDRA_DOWNLOAD_URL) { $env:INDRA_DOWNLOAD_URL } else { "https://github.com/$repo/releases/download" }
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("indra-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "Installing indra $tag for Windows x64 ..."
  Invoke-WebRequest -Uri "$base/$tag/$asset" -OutFile "$tmp\indra.exe" -Headers $headers -UseBasicParsing
  Invoke-WebRequest -Uri "$base/$tag/$asset.sha256" -OutFile "$tmp\indra.sha256" -Headers $headers -UseBasicParsing

  # Check it is exactly the file that was built.
  $want = ((Get-Content "$tmp\indra.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
  $got = (Get-FileHash "$tmp\indra.exe" -Algorithm SHA256).Hash.ToLower()
  if ($want -ne $got) { throw "Checksum mismatch (expected $want, got $got); nothing was installed." }

  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $dest = Join-Path $dir "indra.exe"
  # A running indra.exe cannot be overwritten, but it can be renamed.
  if (Test-Path $dest) { Move-Item -Force $dest "$dest.old" -ErrorAction SilentlyContinue }
  Move-Item -Force "$tmp\indra.exe" $dest
  Remove-Item -Force "$dest.old" -ErrorAction SilentlyContinue
  Write-Host "Installed $(& $dest --version) to $dest"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# On PATH for new terminals (the user's PATH; no administrator needed).
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not $env:INDRA_NO_MODIFY_PATH -and -not (($userPath -split ';') -contains $dir)) {
  [Environment]::SetEnvironmentVariable("Path", (($userPath.TrimEnd(';'), $dir) -join ';').TrimStart(';'), "User")
  $env:Path = "$env:Path;$dir"
  Write-Host "Added $dir to your PATH (new terminals see it)."
}

# Tab completion in PowerShell: a script under indra's data folder, loaded
# from this user's $PROFILE (releases before 0.4.0 have no `completions`).
if (-not $env:INDRA_NO_COMPLETIONS) {
  $env:INDRA_PS_PROFILE = $PROFILE
  & $dest completions powershell --install --quiet *> $null
  if ($LASTEXITCODE -eq 0) { Write-Host "Tab completion is set up for PowerShell (open a new terminal; undo: indra completions powershell --uninstall)" }
  Remove-Item Env:INDRA_PS_PROFILE -ErrorAction SilentlyContinue
}
if ($env:INDRA_AUTO_UPDATE) {
  & $dest update --auto on *> $null
  if ($LASTEXITCODE -eq 0) { Write-Host "Automatic updates are on (turn off: indra update --auto off)" }
}

Write-Host ""
Write-Host "Next: start the console and open it in your browser:"
Write-Host ""
Write-Host "    indra serve --open"
Write-Host ""
Write-Host "Update later with: indra update   (or let it update itself: indra update --auto on)"
