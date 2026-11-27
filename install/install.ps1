# Installs antigravity-notifier, the Antigravity Notifier, on Windows.
#
#   irm https://antigravitynotifier.com/install.ps1 | iex; antigravity-notifier pair 482913
#   irm https://antigravitynotifier.com/install.ps1 | iex; antigravity-notifier activate CCN-XXXX-XXXX-XXXX-XXXX; antigravity-notifier
#                                       # install, use a licence key, then a QR code to pair
#
# `irm | iex` can't pass arguments to the script, so a licence key goes on the same line as its own
# command, as ccn's does (install.sh takes --licence instead).
#
# $env:ANTIGRAVITY_NOTIFIER_VERSION = "0.2.0" picks a release (default: the latest).
# $env:ANTIGRAVITY_NOTIFIER_INSTALL_DIR overrides %LOCALAPPDATA%\Programs\antigravity-notifier.
# $env:ANTIGRAVITY_NOTIFIER_NO_ANALYTICS = "1" (or DO_NOT_TRACK) stops the anonymous install events.
#
# Ported from ccn's install.ps1 (claudecodenotifier.com). Pairing runs next on the same line and installs
# the Antigravity hooks.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$stage = 'start'
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') { 'aarch64' } else { 'x86_64' }

# One anonymous event per step, so we can see where installs fail: the event name, the stage, the
# processor. Nothing else. $env:ANTIGRAVITY_NOTIFIER_NO_ANALYTICS = "1" (or DO_NOT_TRACK) turns it off.
function Send-SetupEvent([string]$name, [hashtable]$properties = @{}) {
    if (($env:ANTIGRAVITY_NOTIFIER_NO_ANALYTICS -and $env:ANTIGRAVITY_NOTIFIER_NO_ANALYTICS -ne '0') -or ($env:DO_NOT_TRACK -and $env:DO_NOT_TRACK -ne '0')) { return }
    $properties['method'] = 'powershell'
    $properties['agent'] = 'antigravity'
    $body = @{
        context = @{ platform = 'cli'; deviceModel = "windows-$arch"; environment = 'production' }
        events = @(@{ name = $name; properties = $properties })
    } | ConvertTo-Json -Depth 5 -Compress
    $server = if ($env:ANTIGRAVITY_NOTIFIER_SERVER_URL) { $env:ANTIGRAVITY_NOTIFIER_SERVER_URL } else { 'https://api.antigravitynotifier.com' }
    try { Invoke-RestMethod -Method Post -Uri "$server/api/events" -ContentType 'application/json' -Headers @{ 'X-App' = 'antigravity' } -Body $body -TimeoutSec 3 | Out-Null } catch { }
}

$repo = 'touch-tap/antigravity-notifier'
$dir = if ($env:ANTIGRAVITY_NOTIFIER_INSTALL_DIR) { $env:ANTIGRAVITY_NOTIFIER_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\antigravity-notifier' }
$asset = "antigravity-notifier-$arch-pc-windows-msvc.zip"
$base = if ($env:ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE) { $env:ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE } elseif ($env:ANTIGRAVITY_NOTIFIER_VERSION) { "https://github.com/$repo/releases/download/v$($env:ANTIGRAVITY_NOTIFIER_VERSION.TrimStart('v'))" } else { "https://github.com/$repo/releases/latest/download" }

Send-SetupEvent 'cli_install_started'

# Set when a download fails: timeout or error
$reason = $null
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("antigravity-notifier-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $stage = 'download'
    Write-Host "Downloading antigravity-notifier for $arch Windows..."
    # A time limit, so a stalled GitHub download (common in mainland China) fails and says so
    # instead of hanging forever
    try {
        Invoke-WebRequest "$base/$asset" -OutFile (Join-Path $tmp $asset) -UseBasicParsing -TimeoutSec 300
        Invoke-WebRequest "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS') -UseBasicParsing -TimeoutSec 300
    } catch {
        # Windows PowerShell: WebException with Status Timeout. PowerShell 7: TaskCanceledException,
        # or a message about the timeout.
        $e = $_.Exception
        $timedOut = ($e -is [System.Net.WebException] -and $e.Status -eq [System.Net.WebExceptionStatus]::Timeout) -or
            ($e -is [System.TimeoutException]) -or ($e -is [System.Threading.Tasks.TaskCanceledException]) -or
            ("$($e.Message) $($e.InnerException.Message)" -match 'timed out|timeout')
        $reason = if ($timedOut) { 'timeout' } else { 'error' }
        if ($timedOut) {
            Write-Host "The download timed out. In mainland China you can install with: npx --registry=https://registry.npmmirror.com @touchtap/antigravity-notifier"
        }
        throw
    }

    $stage = 'checksum'
    $expected = (Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match "\s\*?$([regex]::Escape($asset))$" } | ForEach-Object { ($_ -split '\s+')[0] }) | Select-Object -First 1
    $actual = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLower()
    if (-not $expected -or $expected.ToLower() -ne $actual) { throw "checksum mismatch for $asset" }

    $stage = 'unpack'
    Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    # A running antigravity-notifier.exe cannot be overwritten but can be renamed, so move it aside first
    $target = Join-Path $dir 'antigravity-notifier.exe'
    if (Test-Path $target) {
        $old = Join-Path $dir 'antigravity-notifier.old.exe'
        Remove-Item $old -Force -ErrorAction SilentlyContinue
        Move-Item $target $old -Force
    }
    Move-Item (Join-Path $tmp 'antigravity-notifier.exe') $target
    $stage = 'run'
    Write-Host "Installed $(& $target --version) to $target"
} catch {
    $failed = @{ stage = $stage }
    if ($reason) { $failed['reason'] = $reason }
    Send-SetupEvent 'cli_install_failed' $failed
    throw
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# On PATH for this session (so `antigravity-notifier pair` works on the same line) and for new terminals
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    [Environment]::SetEnvironmentVariable('Path', ($(if ($userPath) { "$userPath;" } else { '' }) + $dir), 'User')
}
if (-not (($env:Path -split ';') -contains $dir)) { $env:Path = "$env:Path;$dir" }

Send-SetupEvent 'cli_install_finished'
# The install line runs `antigravity-notifier pair` next in this same session
$env:ANTIGRAVITY_NOTIFIER_INSTALL_METHOD = 'powershell'

Write-Host ""
Write-Host "Next: antigravity-notifier pair <the code on your phone>, or run antigravity-notifier and scan the QR code."
