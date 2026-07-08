$ErrorActionPreference = "Stop"

$placeholderPattern = '(?i)(CHANGE_ME|YOUR_VALUE|REPLACE_ME|placeholder|example|dummy|test-only|\$\{[A-Z0-9_]+\}|<[^>]+>)'

$rules = @(
    @{ Category = "Ethereum private key assignment"; Pattern = '(?i)\b(PRIVATE_KEY|SIGNER_PRIVATE_KEY|DEPLOYER_KEY)\b\s*[:=]\s*["'']?(0x)?[a-f0-9]{64}\b' },
    @{ Category = "OpenAI API key"; Pattern = '\bsk-(proj-)?[A-Za-z0-9_-]{20,}\b' },
    @{ Category = "GitHub token"; Pattern = '\b(ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})\b' },
    @{ Category = "Telegram bot token"; Pattern = '\b[0-9]{8,10}:[A-Za-z0-9_-]{35,}\b' },
    @{ Category = "Discord or Slack webhook"; Pattern = '(?i)https://(discord(app)?\.com/api/webhooks|hooks\.slack\.com/services)/[^\s"''<>]+' },
    @{ Category = "Authorization header"; Pattern = '(?i)\bAuthorization\b\s*[:=]\s*["'']?(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{20,}' },
    @{ Category = "Bearer token"; Pattern = '(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{30,}' },
    @{ Category = "Basic authorization value"; Pattern = '(?i)\bBasic\s+[A-Za-z0-9+/=]{20,}' },
    @{ Category = "HTTP URL with embedded credentials"; Pattern = '(?i)https?://[^/\s:@]+:[^/\s:@]+@[^/\s]+' },
    @{ Category = "RPC provider API key in URL"; Pattern = '(?i)https?://[^\s"''<>]*(alchemy|infura|quicknode|blastapi|ankr|drpc)[^\s"''<>]*(/v2/|/v3/|api[_-]?key=)[A-Za-z0-9_-]{16,}' },
    @{ Category = "AWS access key"; Pattern = '\b(AKIA|ASIA)[0-9A-Z]{16}\b' },
    @{ Category = "Mnemonic or seed phrase"; Pattern = '(?i)\b(MNEMONIC|SEED_PHRASE|SEED)\b\s*[:=]\s*["'']?([a-z]+[ -]){11,23}[a-z]+' },
    @{ Category = "Real-looking password assignment"; Pattern = '(?i)\b(PASSWORD|PASS|SECRET)\b\s*[:=]\s*["'']?(?!phoenix\b|changeme\b|change_me\b|replace_me\b|your_value\b|placeholder\b|example\b)[^\s"''#]{14,}' }
)

$skipPathPattern = '\\(\.git|target|node_modules|contracts\\out|contracts\\cache|contracts\\broadcast|\.tmp|__pycache__)\\'
$found = $false
$files = Get-ChildItem -Recurse -File | Where-Object {
    $_.FullName -notmatch $skipPathPattern -and $_.Length -lt 5MB
}

foreach ($file in $files) {
    $relative = Resolve-Path -Relative $file.FullName
    $lineNumber = 0
    foreach ($line in [System.IO.File]::ReadLines($file.FullName)) {
        $lineNumber++
        if ($line -match $placeholderPattern) {
            continue
        }
        foreach ($rule in $rules) {
            if ($line -match $rule.Pattern) {
                Write-Output "FILE: $relative"
                Write-Output "LINE: $lineNumber"
                Write-Output "SECRET CATEGORY: $($rule.Category)"
                Write-Output "ACTION REQUIRED: remove from Git candidates and rotate if real"
                Write-Output ""
                $found = $true
                break
            }
        }
    }
}

if ($found) {
    exit 1
}
