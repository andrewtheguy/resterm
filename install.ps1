#!/usr/bin/env pwsh

# resterm installer for Windows
# Downloads latest binary from: https://github.com/andrewtheguy/resterm/releases
#
# Adapted from the beam-rs installer. Flags are read from $args or
# $env:RESTERM_INSTALL_ARGS; there is no param() block, so the script behaves
# the same whether it is run directly or piped into Invoke-Expression.

# Defaults (overwritten by the fallback arg parser)
$ReleaseTag   = $null
$Admin        = $false
$PreRelease   = $false
$DownloadOnly = $false

$ErrorActionPreference = "Stop"

$REPO_OWNER = "andrewtheguy"
$REPO_NAME = "resterm"

# Allow passing flags when the script is piped into Invoke-Expression (iex),
# where normal PowerShell parameter binding is unavailable. Set
# $env:RESTERM_INSTALL_ARGS to a PowerShell-style argument string, e.g.:
#   $env:RESTERM_INSTALL_ARGS='-PreRelease'; irm https://raw.githubusercontent.com/andrewtheguy/resterm/main/install.ps1 | iex

function Print-Info {
    param([string]$Message)
    Write-Host "[INFO] $Message" -ForegroundColor Green
}

function Print-Warn {
    param([string]$Message)
    Write-Host "[WARN] $Message" -ForegroundColor Yellow
}

function Print-Error {
    param([string]$Message)
    Write-Host "[ERROR] $Message" -ForegroundColor Red
}

# Fetch the latest stable release tag (non-prerelease)
function Get-LatestReleaseTag {
    $apiUrl = "https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases/latest"

    try {
        $release = Invoke-RestMethod -Uri $apiUrl -Method Get
    }
    catch {
        Print-Error "Failed to fetch latest release from GitHub: $_"
        exit 1
    }

    if (-not $release.tag_name) {
        Print-Error "Could not find a latest release on GitHub"
        exit 1
    }

    return $release.tag_name
}

# Fetch the latest prerelease tag
function Get-LatestPrereleaseTag {
    $apiUrl = "https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases?per_page=30"

    try {
        $releases = Invoke-RestMethod -Uri $apiUrl -Method Get
    }
    catch {
        Print-Error "Failed to fetch releases from GitHub: $_"
        exit 1
    }

    $latestPrerelease = $releases |
        Where-Object { $_.prerelease -eq $true } |
        Select-Object -First 1 -ExpandProperty tag_name

    if (-not $latestPrerelease) {
        Print-Error "Could not find any prerelease on GitHub"
        exit 1
    }

    return $latestPrerelease
}

# Fetch full release info (including asset checksums) from the GitHub API
function Get-ReleaseInfo {
    param([string]$Tag)

    $apiUrl = "https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases/tags/$Tag"

    try {
        $release = Invoke-RestMethod -Uri $apiUrl -Method Get
        return $release
    }
    catch {
        Print-Warn "Could not fetch release info: $_"
        return $null
    }
}

# Extract the SHA-256 checksum GitHub publishes for a specific asset
function Get-ExpectedChecksum {
    param(
        [object]$ReleaseInfo,
        [string]$BinaryName
    )

    if (-not $ReleaseInfo -or -not $ReleaseInfo.assets) {
        return $null
    }

    $asset = $ReleaseInfo.assets | Where-Object { $_.name -eq $BinaryName } | Select-Object -First 1

    if (-not $asset) {
        return $null
    }

    if ($asset.digest -match 'sha256:([a-f0-9]+)') {
        return $matches[1]
    }

    return $null
}

function Get-FileChecksum {
    param([string]$FilePath)

    try {
        $hash = Get-FileHash -Path $FilePath -Algorithm SHA256
        return $hash.Hash.ToLower()
    }
    catch {
        Print-Error "Failed to compute checksum: $_"
        return $null
    }
}

function Test-Checksum {
    param(
        [string]$FilePath,
        [string]$ExpectedChecksum
    )

    Print-Info "Verifying checksum..."
    $actualChecksum = Get-FileChecksum -FilePath $FilePath

    if (-not $actualChecksum) {
        return $false
    }

    if ($ExpectedChecksum -eq $actualChecksum) {
        $shortHash = $actualChecksum.Substring(0, 16)
        Print-Info "Checksum verified: $shortHash..."
        return $true
    }
    else {
        Print-Error "Checksum verification FAILED!"
        Print-Error "Expected: $ExpectedChecksum"
        Print-Error "Actual:   $actualChecksum"
        return $false
    }
}

# Detect architecture. The release workflow only builds windows-amd64.
function Get-Architecture {
    $arch = [System.Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE")

    if ($arch -ne "AMD64") {
        Print-Error "Unsupported architecture: $arch"
        Print-Error "Only AMD64 (x86_64) is published for Windows; build from source instead:"
        Print-Error "  cargo build --release --features keychain"
        exit 1
    }

    return "amd64"
}

function Get-BinaryName {
    param([string]$Arch)

    if ($Arch -ne "amd64") {
        Print-Error "Unsupported architecture: $Arch"
        exit 1
    }

    return "resterm-windows-amd64.exe"
}

function Get-InstallName {
    return "resterm.exe"
}

# Parse an argument string (e.g. from an environment variable) with PowerShell's
# own tokenizer, so quoting behaves the way a user would expect.
function Parse-ArgString {
    param([string]$ArgString)

    if ([string]::IsNullOrWhiteSpace($ArgString)) {
        return @()
    }

    $errors = $null
    $tokens = [System.Management.Automation.PSParser]::Tokenize($ArgString, [ref]$errors)

    if ($errors -and $errors.Count -gt 0) {
        Print-Warn "Could not parse RESTERM_INSTALL_ARGS: $($errors[0].Message)"
        return @()
    }

    return $tokens |
        Where-Object { $_.Type -in @('CommandArgument', 'CommandParameter', 'String', 'Number') } |
        ForEach-Object { $_.Content }
}

function Apply-FallbackArgs {
    param([string[]]$ArgList)

    if (-not $ArgList -or $ArgList.Count -eq 0) {
        return
    }

    $unhandled = @()

    foreach ($arg in $ArgList) {
        if (-not $arg) { continue }

        $argLower = $arg.ToLowerInvariant()
        switch ($argLower) {
            '-admin' { $script:Admin = $true; continue }
            '/admin' { $script:Admin = $true; continue }
            '-prerelease' { $script:PreRelease = $true; continue }
            '/prerelease' { $script:PreRelease = $true; continue }
            '-downloadonly' { $script:DownloadOnly = $true; continue }
            '/downloadonly' { $script:DownloadOnly = $true; continue }
            '-h' { Show-Usage; exit 0 }
            '/h' { Show-Usage; exit 0 }
            '--help' { Show-Usage; exit 0 }
            '-?' { Show-Usage; exit 0 }
            '/?' { Show-Usage; exit 0 }
            default {
                if (-not $script:ReleaseTag) {
                    $script:ReleaseTag = $arg
                }
                else {
                    $unhandled += $arg
                }
            }
        }
    }

    if ($unhandled.Count -gt 0) {
        Print-Warn "Ignoring unrecognized fallback arguments: $($unhandled -join ' ')"
    }
}

function Download-Binary {
    param(
        [string]$Url,
        [string]$OutputPath,
        [string]$ExpectedChecksum
    )

    Print-Info "Downloading from $Url"

    try {
        Invoke-WebRequest -Uri $Url -OutFile $OutputPath -UseBasicParsing
    }
    catch {
        Print-Error "Failed to download binary: $_"
        exit 1
    }

    if (-not $ExpectedChecksum) {
        Print-Error "No checksum available. Aborting."
        Remove-Item -Path $OutputPath -Force -ErrorAction SilentlyContinue
        exit 1
    }
    if (-not (Test-Checksum -FilePath $OutputPath -ExpectedChecksum $ExpectedChecksum)) {
        Print-Error "Binary integrity check failed. Aborting."
        Remove-Item -Path $OutputPath -Force -ErrorAction SilentlyContinue
        exit 1
    }
}

# `resterm --version` prints and exits before it looks for restic, so this
# smoke test works on a machine that has not installed restic yet.
function Test-Binary {
    param([string]$Path)

    Print-Info "Testing downloaded binary..."
    try {
        $versionInfo = & $Path --version 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "Binary returned non-zero exit code"
        }
        Print-Info "Binary test successful: $versionInfo"
    }
    catch {
        Print-Error "Binary test failed. The downloaded file may be corrupted or incompatible."
        Print-Error "Output: $_"
        Remove-Item -Path $Path -Force -ErrorAction SilentlyContinue
        exit 1
    }
}

# Download only - save to the current directory
function Download-Only {
    param(
        [string]$BaseUrl,
        [string]$BinaryName,
        [string]$ExpectedChecksum
    )

    $url = "$BaseUrl/$BinaryName"
    $outputFile = Join-Path (Get-Location) $BinaryName
    # Stage in a temp directory rather than writing straight to $outputFile. A
    # failed transfer would otherwise leave a truncated binary sitting at the
    # advertised name, and a checksum mismatch would delete whatever file the
    # user already had there. Nothing touches $outputFile until the download is
    # verified and the binary has run.
    $tempDir = Join-Path $env:TEMP "resterm-download-$(Get-Random)"
    $tempBinary = Join-Path $tempDir $BinaryName

    try {
        New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

        Download-Binary -Url $url -OutputPath $tempBinary -ExpectedChecksum $ExpectedChecksum
        Test-Binary -Path $tempBinary

        try {
            Move-Item -Path $tempBinary -Destination $outputFile -Force
        }
        catch {
            Print-Error "Failed to write $($outputFile): $_"
            exit 1
        }

        Print-Info "Binary saved to: $outputFile"
    }
    finally {
        # Runs even on the `exit 1` paths inside Download-Binary/Test-Binary.
        if (Test-Path $tempDir) {
            Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

# Download to a temporary location, test it, then install
function Install-Binary {
    param(
        [string]$BaseUrl,
        [string]$BinaryName,
        [string]$ExpectedChecksum
    )

    $url = "$BaseUrl/$BinaryName"
    $tempDir = Join-Path $env:TEMP "resterm-install-$(Get-Random)"
    $tempBinary = Join-Path $tempDir $BinaryName
    $installDir = Join-Path $env:LOCALAPPDATA "Programs\resterm"
    $installName = Get-InstallName
    $finalPath = Join-Path $installDir $installName

    try {
        New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

        Download-Binary -Url $url -OutputPath $tempBinary -ExpectedChecksum $ExpectedChecksum
        Test-Binary -Path $tempBinary

        if (-not (Test-Path $installDir)) {
            New-Item -ItemType Directory -Path $installDir -Force | Out-Null
        }

        try {
            Move-Item -Path $tempBinary -Destination $finalPath -Force
        }
        catch {
            Print-Error "Failed to move binary to final location: $_"
            Print-Error "If resterm is currently running, close it and try again."
            exit 1
        }

        Print-Info "Binary installed successfully to $finalPath"

        # Add to PATH if not already there
        $userPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
        if ($userPath -notlike "*$installDir*") {
            Print-Warn "$installDir is not in your PATH"
            Print-Warn "Adding to user PATH..."

            try {
                $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
                [System.Environment]::SetEnvironmentVariable("Path", $newPath, "User")
                Print-Info "Added to PATH. You may need to restart your terminal for changes to take effect."
            }
            catch {
                Print-Warn "Failed to add to PATH automatically. Please add manually:"
                Print-Warn "$installDir"
            }
        }
        else {
            Print-Info "$installDir is already in your PATH"
        }
    }
    finally {
        if (Test-Path $tempDir) {
            Remove-Item -Path $tempDir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

# resterm drives the restic CLI; without it the TUI refuses to start.
function Test-ResticPresent {
    $restic = Get-Command restic -ErrorAction SilentlyContinue
    if (-not $restic) {
        Print-Warn "restic was not found on your PATH."
        Print-Warn "resterm requires restic >= 0.19 to run. Install it with:"
        Print-Warn "  winget install restic.restic"
        Print-Warn "or grab a binary from https://github.com/restic/restic/releases"
        return
    }

    $versionLine = (& $restic.Source version 2>&1 | Select-Object -First 1)
    Print-Info "Found restic: $versionLine"
    if ($versionLine -match 'restic\s+(\d+)\.(\d+)\.(\d+)') {
        $found = [version]::new([int]$matches[1], [int]$matches[2], [int]$matches[3])
        if ($found -lt [version]::new(0, 19, 0)) {
            Print-Warn "resterm requires restic >= 0.19; run 'restic self-update' to upgrade."
        }
    }
}

function Show-Usage {
    Write-Host @"
Usage: .\install.ps1 [OPTIONS] [RELEASE_TAG]

Download and install the resterm binary

Options:
  -DownloadOnly  Download binary to current directory without installing
  -PreRelease    Use latest prerelease tag instead of latest stable release
  -Admin         Allow installation with administrator privileges (not recommended)
  -h, --help     Show this help message

Arguments:
  RELEASE_TAG    GitHub release tag to download (default: latest)

Environment variables:
  `$env:RELEASE_TAG           Alternative way to specify release tag
  `$env:RESTERM_INSTALL_ARGS  Fallback flags for iex one-liners (e.g. "-PreRelease")

Examples:
  .\install.ps1                              # Install latest resterm
  .\install.ps1 <release-tag>                # Install specific stable release
  .\install.ps1 -DownloadOnly                # Download latest to current directory
  .\install.ps1 -DownloadOnly <release-tag>  # Download specific stable release
  .\install.ps1 -Admin                       # Allow admin installation (not recommended)
  `$env:RELEASE_TAG='v0.0.1'; .\install.ps1  # Use environment variable

Installs to: `$env:LOCALAPPDATA\Programs\resterm (added to the user PATH)

Supported platforms: Windows (amd64). The Windows build ships with the
'keychain' feature enabled, so passphrases can be saved to Windows Credential
Manager.

Note: resterm also needs restic >= 0.19 on PATH ('winget install restic.restic').
Note: Installation as administrator is not recommended. Use -Admin to override.
"@
}

function Test-AdminPrivileges {
    param([bool]$AllowAdmin)

    $currentPrincipal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    $isAdmin = $currentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

    if ($isAdmin) {
        if (-not $AllowAdmin) {
            Print-Error "Installation as administrator is not allowed without explicit override."
            Print-Error "Running as administrator can cause permission issues and is not recommended."
            Print-Error ""
            Print-Error "To proceed anyway, run with the -Admin flag:"
            Print-Error "  .\install.ps1 -Admin"
            Print-Error ""
            Print-Error "Recommended: Run this installer as a regular user instead."
            exit 1
        }
        else {
            Print-Warn "Running as administrator with explicit override (-Admin flag)."
            Print-Warn "This is not recommended and may cause permission issues."
        }
    }
}

function Start-Installation {
    param(
        [string]$Tag,
        [bool]$DownloadOnly
    )

    if ($DownloadOnly) {
        Print-Info "resterm downloader"
    }
    else {
        Print-Info "resterm installer"
    }
    Print-Info "Release: $Tag"
    Print-Info "Repository: $REPO_OWNER/$REPO_NAME"

    $arch = Get-Architecture
    $binaryName = Get-BinaryName -Arch $arch

    Print-Info "Platform detected: windows-$arch"
    Print-Info "Binary name: $binaryName"

    $baseUrl = "https://github.com/$REPO_OWNER/$REPO_NAME/releases/download/$Tag"

    Print-Info "Fetching release information..."
    $releaseInfo = Get-ReleaseInfo -Tag $Tag

    if (-not $releaseInfo) {
        Print-Error "Could not fetch release info from GitHub. Cannot verify binary integrity."
        exit 1
    }

    $expectedChecksum = Get-ExpectedChecksum -ReleaseInfo $releaseInfo -BinaryName $binaryName
    if (-not $expectedChecksum) {
        Print-Error "No checksum found for $binaryName in release $Tag. Cannot verify binary integrity."
        exit 1
    }
    $shortHash = $expectedChecksum.Substring(0, 16)
    Print-Info "Expected checksum: $shortHash..."

    if ($DownloadOnly) {
        Download-Only -BaseUrl $baseUrl -BinaryName $binaryName -ExpectedChecksum $expectedChecksum
        Print-Info "Download completed successfully!"
    }
    else {
        Install-Binary -BaseUrl $baseUrl -BinaryName $binaryName -ExpectedChecksum $expectedChecksum
        Print-Info "Installation completed successfully!"
        $installName = Get-InstallName
        Print-Info "You can now run '$installName' from your terminal."
        Test-ResticPresent
    }
}

function Main {
    # Arguments arrive either from the caller (forwarded via `Main @args`) or
    # from the environment, which is the only channel an `iex` one-liner has.
    $fallbackArgs = @()
    if ($args -and $args.Count -gt 0) {
        $fallbackArgs += $args
    }
    if ($env:RESTERM_INSTALL_ARGS) {
        $fallbackArgs += (Parse-ArgString -ArgString $env:RESTERM_INSTALL_ARGS)
    }
    Apply-FallbackArgs -ArgList $fallbackArgs

    # Extra guard: honor env flags even if tokenization failed.
    $envArgs = $env:RESTERM_INSTALL_ARGS
    if ($envArgs) {
        if (-not $script:PreRelease -and $envArgs -match '(?i)(^|\s)--?prerelease(\s|$)') { $script:PreRelease = $true }
        if (-not $script:DownloadOnly -and $envArgs -match '(?i)(^|\s)--?downloadonly(\s|$)') { $script:DownloadOnly = $true }
        if (-not $script:Admin -and $envArgs -match '(?i)(^|\s)--?admin(\s|$)') { $script:Admin = $true }
        if (-not $script:ReleaseTag -and $envArgs -match '^(\s*[^-][^\s]+)') {
            $script:ReleaseTag = $matches[1].Trim()
        }
    }

    if ($script:DownloadOnly) {
        Print-Info "Starting resterm download..."
    }
    else {
        Print-Info "Starting resterm installation..."
    }

    # Determine release tag
    $tag = $script:ReleaseTag
    if (-not $tag) {
        $tag = $env:RELEASE_TAG
    }
    if (-not $tag) {
        if ($script:PreRelease) {
            Print-Info "Fetching latest prerelease tag from GitHub..."
            $tag = Get-LatestPrereleaseTag
        }
        else {
            Print-Info "Fetching latest release tag from GitHub..."
            $tag = Get-LatestReleaseTag
        }
    }

    if (-not $script:DownloadOnly) {
        Test-AdminPrivileges -AllowAdmin:$script:Admin
    }

    Start-Installation -Tag $tag -DownloadOnly:$script:DownloadOnly
}

# `@args` forwards the script's own arguments, so `.\install.ps1 -DownloadOnly`
# works as well as the env-var channel used by `iex`.
#
# RESTERM_INSTALL_ARGS is read but never unset: the caller created it, so it is
# theirs to clear. (`$env:` variables are process-scoped anyway — they are
# inherited by child processes but invisible to any later session, so there is
# nothing to "leak".)
Main @args
