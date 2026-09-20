#Requires -Version 5.1
<#
.SYNOPSIS
  Installs the NoralWeb terminal client: `noral` works from anywhere.
.DESCRIPTION
  Downloads noral-cli from the GitHub release into LOCALAPPDATA\NoralWeb\bin
  as noral.exe and adds it to the user PATH. No more double-clicking an exe:
  type `noral "sorgu"` in a terminal (opencode-style).
  Turkish notes: kurulum dizini + PATH islemi otomatiktir.
.PARAMETER InstallDir
  Install directory (change for tests).
.PARAMETER NoPath
  Skip PATH change (tests / locked environments).
.EXAMPLE
  irm https://raw.githubusercontent.com/samansarmasik-alt/NoralWeb/main/install.ps1 | iex
#>
param(
  [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "NoralWeb\bin"),
  [switch]$NoPath
)
$ErrorActionPreference = "Stop"
$Tag = "v0.37.0"
$Asset = "noral-cli-v37.exe"
$Url = "https://github.com/samansarmasik-alt/NoralWeb/releases/download/$Tag/$Asset"
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$Dest = Join-Path $InstallDir "noral.exe"
Write-Output "indirilen: $Url"
Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
Write-Output "kuruldu: $Dest"
& $Dest --version
if (-not $NoPath) {
  $cur = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($cur -split ";" -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable("Path", "$cur;$InstallDir", "User")
    Write-Output "PATH eklendi (yeni terminalde gecerli): $InstallDir"
  } else {
    Write-Output "PATH icinde zaten var."
  }
}
Write-Output 'kullanim: noral "sorgu"  |  noral --agent "soru"  |  noral --testmode'
