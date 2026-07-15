$ErrorActionPreference = "Stop"

$patterns = @(
    @{ Category = "accidental Git runtime state"; Pattern = '^FETCH_HEAD$' },
    @{ Category = "environment file"; Pattern = '(^|[\\/])\.env($|[\\/])|(^|[\\/])\.env\.local$|(^|[\\/])\.env\..*\.local$' },
    @{ Category = "private key or certificate"; Pattern = '\.(pem|key|pfx|p12|jks|keystore)$' },
    @{ Category = "keystore directory"; Pattern = '(^|[\\/])(keystore|keystores|wallets)([\\/]|$)' },
    @{ Category = "wallet export"; Pattern = '(wallet.*\.(json|txt|wallet)$|UTC--.*)' },
    @{ Category = "local database"; Pattern = '\.(db|sqlite|sqlite3|db-wal|db-shm)$' },
    @{ Category = "feed recording output"; Pattern = '(^|[\\/])(recordings|feed-recordings)([\\/]|$)|\.(ndjson|jsonl)\.zst$' },
    @{ Category = "replay or benchmark output"; Pattern = '(^|[\\/])(replay-output|benchmark-output|bench-output)([\\/]|$)|\.(prof|pprof|bench)$' },
    @{ Category = "runtime data directory"; Pattern = '(^|[\\/])(postgres-data|postgres_data|pgdata|prometheus-data|prometheus_data|\.tmp|tmp|temp)([\\/]|$)' },
    @{ Category = "build output"; Pattern = '(^|[\\/])(target|out|cache|broadcast|dist|build|node_modules|__pycache__)([\\/]|$)|\.(pyc|exe|test|out)$' }
)

$candidates = git ls-files --cached --others --exclude-standard
$found = $false

foreach ($path in $candidates) {
    $normalized = $path -replace '\\', '/'
    if ($normalized -eq ".env.example") {
        continue
    }
    if ($normalized -match '^fixtures/') {
        continue
    }
    foreach ($rule in $patterns) {
        if ($normalized -match $rule.Pattern) {
            Write-Output "FILE: $path"
            Write-Output "FORBIDDEN CATEGORY: $($rule.Category)"
            Write-Output "ACTION REQUIRED: remove from tracked candidates or update ignore policy"
            Write-Output ""
            $found = $true
            break
        }
    }
}

if ($found) {
    exit 1
}
