# Font licences for Windows packages, dot-sourced by package.ps1 (the portable zip and the MSI).
# Builds made with craft-fonts (CRAFT_FONTS_DIR, set for every release) embed its fonts, so the
# packages carry their licences: the `licence file` field (6th) of each fonts\manifest.txt line,
# named after its directory (OFL.txt -> OFL-<dir>.txt; Droid Sans Fallback's Apache-2.0 NOTICE ->
# NOTICE-droid-sans-fallback.txt). Same rule as copy_font_licences in packaging/env.sh; both are
# tested by packaging/tests/font-licences.{sh,ps1}.

# A licence that can't be copied fails a release build (CRAFT_FONTS_REQUIRED set, as for build.rs)
# and is a warning otherwise.
function Write-FontLicenceProblem([string] $Message) {
  if (Test-Path Env:CRAFT_FONTS_REQUIRED) { throw "$Message (CRAFT_FONTS_REQUIRED is set)" }
  Write-Warning $Message
}

# Copy each embedded font's licence file into $Dest. Returns the names of the files written.
function Copy-FontLicences([Parameter(Mandatory)] [string] $Dest) {
  if (-not $env:CRAFT_FONTS_DIR) { return @() }
  $manifest = Join-Path $env:CRAFT_FONTS_DIR 'fonts\manifest.txt'
  if (-not (Test-Path $manifest -PathType Leaf)) {
    Write-FontLicenceProblem "CRAFT_FONTS_DIR=$env:CRAFT_FONTS_DIR has no fonts\manifest.txt; no font licences copied"
    return @()
  }
  $licences = Get-Content $manifest | ForEach-Object { $_.Trim() } |
    Where-Object { $_ -and -not $_.StartsWith('#') } |
    ForEach-Object { $fields = $_ -split ' \| '; if ($fields.Count -ge 6) { $fields[5].Trim() } } |
    Sort-Object -Unique
  $written = @()
  foreach ($rel in $licences) {
    if ([IO.Path]::IsPathRooted($rel) -or $rel.Contains('..')) {
      Write-FontLicenceProblem "font licence path $rel leaves the checkout; skipped"
      continue
    }
    $src = Join-Path $env:CRAFT_FONTS_DIR $rel
    if (-not (Test-Path $src -PathType Leaf)) {
      Write-FontLicenceProblem "font licence $rel is listed in the craft-fonts manifest but missing"
      continue
    }
    $dir = Split-Path (Split-Path $rel -Parent) -Leaf
    $name = '{0}-{1}.txt' -f [IO.Path]::GetFileNameWithoutExtension($rel), $dir
    Copy-Item $src (Join-Path $Dest $name)
    $written += $name
  }
  return $written
}
