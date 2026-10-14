<#
.SYNOPSIS
  Tests Copy-FontLicences (packaging/windows/font-licences.ps1), which fills licences\ in the MSI and
  the portable zip, against the same synthetic craft-fonts checkouts as font-licences.sh.
.EXAMPLE
  pwsh packaging/tests/font-licences.ps1
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../windows/font-licences.ps1')

function Fail([string] $Message) { throw "FAIL: $Message" }

$Work = Join-Path ([IO.Path]::GetTempPath()) "pdfcraft-font-licences-$PID"
$Saved = @{ Dir = $env:CRAFT_FONTS_DIR; Required = $env:CRAFT_FONTS_REQUIRED }
Remove-Item Env:CRAFT_FONTS_DIR, Env:CRAFT_FONTS_REQUIRED -ErrorAction SilentlyContinue
$Sha = '0' * 64

function New-Checkout([string] $Dir, [switch] $Broken) {
  foreach ($d in 'serif', 'sans', 'fallback') { New-Item -ItemType Directory -Force -Path (Join-Path $Dir "fonts/$d") | Out-Null }
  Set-Content -NoNewline -Path (Join-Path $Dir 'fonts/serif/OFL.txt') -Value "SIL OPEN FONT LICENSE Version 1.1 (serif)`n"
  Set-Content -NoNewline -Path (Join-Path $Dir 'fonts/sans/OFL.txt') -Value "SIL OPEN FONT LICENSE Version 1.1 (sans)`n"
  Set-Content -NoNewline -Path (Join-Path $Dir 'fonts/fallback/NOTICE') -Value "Apache License Version 2.0 (fallback)`n"
  $lines = @(
    '# family | style | file | scripts | licence | licence file | sha256 | source'
    "Serif | Regular | fonts/serif/Serif.ttf | Jpan,Latn | OFL-1.1 | fonts/serif/OFL.txt | $Sha | https://example.com/serif"
    ''
    "Sans | Regular | fonts/sans/Sans-Regular.ttf | Jpan,Latn | OFL-1.1 | fonts/sans/OFL.txt | $Sha | https://example.com/sans"
    "Sans | Bold | fonts/sans/Sans-Bold.ttf | Jpan,Latn | OFL-1.1 | fonts/sans/OFL.txt | $Sha | https://example.com/sans"
    "Fallback | Regular | fonts/fallback/Fallback.ttf | Hans,Hant | Apache-2.0 | fonts/fallback/NOTICE | $Sha | https://example.com/fallback"
  )
  if ($Broken) {
    $lines += "Broken | Regular | fonts/broken/B.ttf | Latn | OFL-1.1 | fonts/broken/OFL.txt | $Sha | https://example.com"
    $lines += "Escape | Regular | fonts/x/E.ttf | Latn | OFL-1.1 | ../outside.txt | $Sha | https://example.com"
  }
  Set-Content -Path (Join-Path $Dir 'fonts/manifest.txt') -Value $lines
}

function New-Out([string] $Name) { $d = Join-Path $Work $Name; New-Item -ItemType Directory -Force -Path $d | Out-Null; $d }

try {
  # 1. Every licence file, once, named after its directory, byte-identical; the names are returned.
  New-Checkout (Join-Path $Work 'good')
  $env:CRAFT_FONTS_DIR = Join-Path $Work 'good'
  $out = New-Out 'out1'
  $names = @(Copy-FontLicences $out) | Sort-Object
  $got = @(Get-ChildItem $out | ForEach-Object Name | Sort-Object)
  if (($got -join ' ') -ne 'NOTICE-fallback.txt OFL-sans.txt OFL-serif.txt') { Fail "copied: $($got -join ' ')" }
  if (($names -join ' ') -ne ($got -join ' ')) { Fail "returned: $($names -join ' ')" }
  if ((Get-FileHash (Join-Path $out 'NOTICE-fallback.txt')).Hash -ne (Get-FileHash (Join-Path $Work 'good/fonts/fallback/NOTICE')).Hash) { Fail 'NOTICE content differs' }

  # 2. Without CRAFT_FONTS_DIR nothing is copied and nothing fails.
  Remove-Item Env:CRAFT_FONTS_DIR
  $out = New-Out 'out2'
  if (@(Copy-FontLicences $out).Count -ne 0 -or @(Get-ChildItem $out).Count -ne 0) { Fail 'copied without CRAFT_FONTS_DIR' }

  # 3. A missing licence file and a path out of the checkout: warnings for a local build, which still
  #    copies the rest, and an error for a release build.
  New-Checkout (Join-Path $Work 'bad') -Broken
  Set-Content -Path (Join-Path $Work 'outside.txt') -Value 'not a licence'
  $env:CRAFT_FONTS_DIR = Join-Path $Work 'bad'
  $out = New-Out 'out3'
  $warnings = @(Copy-FontLicences $out 3>&1 | Where-Object { $_ -is [System.Management.Automation.WarningRecord] } | ForEach-Object Message)
  if (-not ($warnings -match 'fonts/broken/OFL.txt is listed in the craft-fonts manifest but missing')) { Fail "no missing-licence warning: $warnings" }
  if (-not ($warnings -match '\.\./outside\.txt leaves the checkout')) { Fail "no path warning: $warnings" }
  if (@(Get-ChildItem $out).Count -ne 3) { Fail 'a local build did not copy the good licences' }
  $env:CRAFT_FONTS_REQUIRED = '1'
  $threw = $false
  try { Copy-FontLicences (New-Out 'out4') | Out-Null } catch { $threw = $true }
  if (-not $threw) { Fail 'a release build accepted a missing font licence' }

  # 4. A checkout without a manifest: a warning, or an error for a release build.
  $env:CRAFT_FONTS_DIR = New-Out 'empty'
  $threw = $false
  try { Copy-FontLicences (New-Out 'out5') | Out-Null } catch { $threw = $true }
  if (-not $threw) { Fail 'a release build accepted a checkout without a manifest' }
  Remove-Item Env:CRAFT_FONTS_REQUIRED
  Copy-FontLicences (New-Out 'out6') 3>$null | Out-Null

  Write-Output 'ok font licences (PowerShell): manifest names, Apache NOTICE, warnings locally, errors with CRAFT_FONTS_REQUIRED'
} finally {
  Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
  if ($null -ne $Saved.Dir) { $env:CRAFT_FONTS_DIR = $Saved.Dir } else { Remove-Item Env:CRAFT_FONTS_DIR -ErrorAction SilentlyContinue }
  if ($null -ne $Saved.Required) { $env:CRAFT_FONTS_REQUIRED = $Saved.Required } else { Remove-Item Env:CRAFT_FONTS_REQUIRED -ErrorAction SilentlyContinue }
}
