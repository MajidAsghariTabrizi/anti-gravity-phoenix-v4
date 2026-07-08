#!/usr/bin/env sh
set -eu

placeholder_re='(CHANGE_ME|YOUR_VALUE|REPLACE_ME|placeholder|example|dummy|test-only|\$\{[A-Z0-9_]+\}|<[^>]+>)'
found_file="$(mktemp)"
trap 'rm -f "$found_file"' EXIT

scan_rule() {
  file="$1"
  category="$2"
  pattern="$3"
  result="$(grep -nE "$pattern" "$file" 2>/dev/null | grep -Ev "$placeholder_re" | head -n 1 || true)"
  if [ -n "$result" ]; then
    line="${result%%:*}"
    printf 'FILE: %s\n' "$file"
    printf 'LINE: %s\n' "$line"
    printf 'SECRET CATEGORY: %s\n' "$category"
    printf 'ACTION REQUIRED: remove from Git candidates and rotate if real\n\n'
    echo 1 > "$found_file"
  fi
}

find . -type f \
  ! -path './.git/*' \
  ! -path './target/*' \
  ! -path './node_modules/*' \
  ! -path './contracts/out/*' \
  ! -path './contracts/cache/*' \
  ! -path './contracts/broadcast/*' \
  ! -path './.tmp/*' \
  ! -path '*/__pycache__/*' \
  -print | while IFS= read -r file; do
  grep -Iq . "$file" || continue
  scan_rule "$file" "Ethereum private key assignment" '\b(PRIVATE_KEY|SIGNER_PRIVATE_KEY|DEPLOYER_KEY)\b[[:space:]]*[:=][[:space:]]*["'\'']?(0x)?[a-fA-F0-9]{64}\b'
  scan_rule "$file" "OpenAI API key" '\bsk-(proj-)?[A-Za-z0-9_-]{20,}\b'
  scan_rule "$file" "GitHub token" '\b(ghp_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})\b'
  scan_rule "$file" "Telegram bot token" '\b[0-9]{8,10}:[A-Za-z0-9_-]{35,}\b'
  scan_rule "$file" "Discord or Slack webhook" 'https://(discord(app)?\.com/api/webhooks|hooks\.slack\.com/services)/[^[:space:]"'\''<>]+'
  scan_rule "$file" "Authorization header" '\bAuthorization\b[[:space:]]*[:=][[:space:]]*["'\'']?(Bearer|Basic)[[:space:]]+[A-Za-z0-9._~+/=-]{20,}'
  scan_rule "$file" "Bearer token" '\bBearer[[:space:]]+[A-Za-z0-9._~+/=-]{30,}'
  scan_rule "$file" "Basic authorization value" '\bBasic[[:space:]]+[A-Za-z0-9+/=]{20,}'
  scan_rule "$file" "HTTP URL with embedded credentials" 'https?://[^/[:space:]:@]+:[^/[:space:]:@]+@[^/[:space:]]+'
  scan_rule "$file" "RPC provider API key in URL" 'https?://[^[:space:]"'\''<>]*(alchemy|infura|quicknode|blastapi|ankr|drpc)[^[:space:]"'\''<>]*(/v2/|/v3/|api[_-]?key=)[A-Za-z0-9_-]{16,}'
  scan_rule "$file" "AWS access key" '\b(AKIA|ASIA)[0-9A-Z]{16}\b'
  scan_rule "$file" "Mnemonic or seed phrase" '\b(MNEMONIC|SEED_PHRASE|SEED)\b[[:space:]]*[:=][[:space:]]*["'\'']?([a-z]+[ -]){11,23}[a-z]+'
  scan_rule "$file" "Real-looking password assignment" '\b(PASSWORD|PASS|SECRET)\b[[:space:]]*[:=][[:space:]]*["'\'']?[^[:space:]"'\''#]{14,}'
done

if [ -s "$found_file" ]; then
  exit 1
fi
