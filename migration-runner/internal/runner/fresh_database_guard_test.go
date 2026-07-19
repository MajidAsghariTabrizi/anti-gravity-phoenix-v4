package runner

import (
	"fmt"
	"net"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"unicode"
)

var freshDatabaseTestNamePattern = regexp.MustCompile(`^phoenix_v5_fresh_test_[0-9]+$`)

func TestFreshDatabaseDSNSanitizerAcceptsCanonicalLoopbackURLs(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		dsn  string
		want string
	}{
		{
			name: "postgres IPv4 with port",
			dsn:  "postgres://user@127.0.0.1:5432/postgres?sslmode=disable",
			want: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable",
		},
		{
			name: "postgresql localhost with password",
			dsn:  "postgresql://user:password@localhost/postgres?sslmode=disable",
			want: "postgresql://user:password@localhost/postgres?sslmode=disable",
		},
		{
			name: "postgres localhost without port",
			dsn:  "postgres://user@localhost/phoenix_test?sslmode=disable",
			want: "postgres://user@localhost/phoenix_test?sslmode=disable",
		},
	}

	for _, test := range tests {
		test := test
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			sanitized, err := sanitizeFreshDatabaseTestDSN(test.dsn)
			if err != nil {
				t.Fatalf("sanitize canonical DSN: %v", err)
			}
			if got := sanitized.String(); got != test.want {
				t.Fatalf("sanitized DSN = %q, want %q", got, test.want)
			}
		})
	}
}

func TestFreshDatabaseDSNSanitizerRejectsUnsafeURLs(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		dsn  string
	}{
		{name: "query host override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&host=remote.example"},
		{name: "encoded query host override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&%68ost=remote.example"},
		{name: "hostaddr override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&hostaddr=203.0.113.10"},
		{name: "port override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&port=6432"},
		{name: "database override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&dbname=remote"},
		{name: "user override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&user=other"},
		{name: "password override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&password=other"},
		{name: "service override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&service=remote"},
		{name: "servicefile override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&servicefile=/tmp/pg_service.conf"},
		{name: "passfile override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&passfile=/tmp/pgpass"},
		{name: "options override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&options=-csearch_path=other"},
		{name: "target session attrs override", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&target_session_attrs=read-write"},
		{name: "unknown query parameter", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&connect_timeout=5"},
		{name: "repeated sslmode", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=disable&sslmode=disable"},
		{name: "empty sslmode", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode="},
		{name: "non-disabled sslmode", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=require"},
		{name: "missing sslmode", dsn: "postgres://user@127.0.0.1:5432/postgres"},
		{name: "encoded sslmode key", dsn: "postgres://user@127.0.0.1:5432/postgres?ssl%6dode=disable"},
		{name: "encoded sslmode value", dsn: "postgres://user@127.0.0.1:5432/postgres?sslmode=%64isable"},
		{name: "remote authority", dsn: "postgres://user@remote.example:5432/postgres?sslmode=disable"},
		{name: "comma-separated authority", dsn: "postgres://user@localhost,remote.example:5432/postgres?sslmode=disable"},
		{name: "Unix socket authority", dsn: "postgres://user@%2Fvar%2Frun%2Fpostgresql/postgres?sslmode=disable"},
		{name: "Unix socket query", dsn: "postgres://user@localhost/postgres?sslmode=disable&host=/var/run/postgresql"},
		{name: "malformed port", dsn: "postgres://user@localhost:not-a-port/postgres?sslmode=disable"},
		{name: "port zero", dsn: "postgres://user@localhost:0/postgres?sslmode=disable"},
		{name: "port above range", dsn: "postgres://user@localhost:65536/postgres?sslmode=disable"},
		{name: "fragment", dsn: "postgres://user@localhost/postgres?sslmode=disable#unsafe"},
		{name: "opaque URL", dsn: "postgres:user@localhost/postgres?sslmode=disable"},
		{name: "empty hostname", dsn: "postgres:///postgres?sslmode=disable"},
		{name: "IPv6 authority", dsn: "postgres://user@[::1]:5432/postgres?sslmode=disable"},
		{name: "whitespace", dsn: "postgres://user@localhost/postgres?sslmode=disable "},
	}

	for _, test := range tests {
		test := test
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if sanitized, err := sanitizeFreshDatabaseTestDSN(test.dsn); err == nil {
				t.Fatalf("unsafe DSN unexpectedly sanitized to %q", sanitized)
			}
		})
	}
}

func TestFreshDatabaseDSNGuardRejectsQueryHostBeforeConnection(t *testing.T) {
	t.Parallel()
	destructivePathReached := false
	err := withValidatedFreshDatabaseTestDSN(
		"postgres://user@127.0.0.1:5432/postgres?sslmode=disable&host=remote.example",
		func(*url.URL) {
			destructivePathReached = true
		},
	)
	if err == nil {
		t.Fatal("query host override was accepted")
	}
	if destructivePathReached {
		t.Fatal("connection or database mutation path was reached before DSN rejection")
	}
}

func TestFreshDatabaseGeneratedNameGuard(t *testing.T) {
	t.Parallel()
	if err := validateFreshDatabaseTestName("phoenix_v5_fresh_test_123456789"); err != nil {
		t.Fatalf("generated database name rejected: %v", err)
	}
	for _, databaseName := range []string{
		"phoenix_v5_fresh_test_",
		"phoenix_v5_fresh_test_remote",
		"phoenix_v5_fresh_test_1;DROP DATABASE postgres",
		"postgres",
	} {
		if err := validateFreshDatabaseTestName(databaseName); err == nil {
			t.Fatalf("unsafe database name %q was accepted", databaseName)
		}
	}
}

func sanitizeFreshDatabaseTestDSN(rawDSN string) (*url.URL, error) {
	if rawDSN == "" {
		return nil, fmt.Errorf("migration test DSN is empty")
	}
	if containsWhitespace(rawDSN) {
		return nil, fmt.Errorf("migration test DSN must not contain whitespace")
	}

	parsed, err := url.Parse(rawDSN)
	if err != nil {
		return nil, fmt.Errorf("parse migration test DSN: %w", err)
	}
	if parsed.Scheme != "postgres" && parsed.Scheme != "postgresql" {
		return nil, fmt.Errorf("migration test DSN must use PostgreSQL")
	}
	if parsed.Opaque != "" {
		return nil, fmt.Errorf("migration test DSN must not use opaque URL form")
	}
	if parsed.Fragment != "" || parsed.RawFragment != "" {
		return nil, fmt.Errorf("migration test DSN must not contain a fragment")
	}
	if parsed.Host == "" || parsed.Hostname() == "" {
		return nil, fmt.Errorf("migration test DSN must include a hostname")
	}
	if strings.Contains(parsed.Host, ",") {
		return nil, fmt.Errorf("migration test DSN must contain exactly one host")
	}

	hostname := parsed.Hostname()
	if hostname != "127.0.0.1" && hostname != "localhost" {
		return nil, fmt.Errorf("fresh database integration test is loopback-only")
	}

	port := parsed.Port()
	canonicalAuthority := hostname
	if port != "" {
		if !isASCIIUnsignedInteger(port) {
			return nil, fmt.Errorf("migration test DSN port must be numeric")
		}
		portNumber, err := strconv.Atoi(port)
		if err != nil || portNumber < 1 || portNumber > 65535 {
			return nil, fmt.Errorf("migration test DSN port must be between 1 and 65535")
		}
		port = strconv.Itoa(portNumber)
		canonicalAuthority = net.JoinHostPort(hostname, port)
	}
	if parsed.Host != canonicalAuthority {
		return nil, fmt.Errorf("migration test DSN authority is not canonical loopback host and port")
	}

	if parsed.RawPath != "" {
		return nil, fmt.Errorf("migration test DSN database path must not be encoded")
	}
	databaseName := strings.TrimPrefix(parsed.Path, "/")
	if !strings.HasPrefix(parsed.Path, "/") ||
		databaseName == "" ||
		strings.Contains(databaseName, "/") ||
		strings.ContainsRune(databaseName, '\x00') ||
		containsWhitespace(databaseName) {
		return nil, fmt.Errorf("migration test DSN must contain one valid database path segment")
	}

	if parsed.User != nil {
		if containsWhitespace(parsed.User.Username()) {
			return nil, fmt.Errorf("migration test DSN user information must not contain whitespace")
		}
		if password, present := parsed.User.Password(); present && containsWhitespace(password) {
			return nil, fmt.Errorf("migration test DSN user information must not contain whitespace")
		}
	}

	query, err := url.ParseQuery(parsed.RawQuery)
	if err != nil {
		return nil, fmt.Errorf("parse migration test DSN query: %w", err)
	}
	if len(query) != 1 {
		return nil, fmt.Errorf("migration test DSN query must contain only sslmode=disable")
	}
	for key := range query {
		if key != "sslmode" {
			return nil, fmt.Errorf("migration test DSN query parameter %q is not allowed", key)
		}
	}
	sslmodeValues := query["sslmode"]
	if len(sslmodeValues) != 1 || sslmodeValues[0] != "disable" {
		return nil, fmt.Errorf("migration test DSN requires exactly one sslmode=disable value")
	}
	rawQueryParts := strings.Split(parsed.RawQuery, "&")
	if len(rawQueryParts) != 1 {
		return nil, fmt.Errorf("migration test DSN requires exactly one query parameter")
	}
	rawKey, rawValue, hasValue := strings.Cut(rawQueryParts[0], "=")
	if !hasValue || rawKey != "sslmode" || rawValue != "disable" {
		return nil, fmt.Errorf("migration test DSN query must be canonical sslmode=disable")
	}

	sanitized := &url.URL{
		Scheme:   parsed.Scheme,
		Host:     canonicalAuthority,
		Path:     "/" + databaseName,
		RawQuery: "sslmode=disable",
	}
	if parsed.User != nil {
		username := parsed.User.Username()
		if password, present := parsed.User.Password(); present {
			sanitized.User = url.UserPassword(username, password)
		} else {
			sanitized.User = url.User(username)
		}
	}
	return sanitized, nil
}

func withValidatedFreshDatabaseTestDSN(rawDSN string, run func(*url.URL)) error {
	sanitized, err := sanitizeFreshDatabaseTestDSN(rawDSN)
	if err != nil {
		return err
	}
	run(sanitized)
	return nil
}

func validateFreshDatabaseTestName(databaseName string) error {
	if !freshDatabaseTestNamePattern.MatchString(databaseName) {
		return fmt.Errorf("fresh database test name is outside the generated namespace")
	}
	return nil
}

func containsWhitespace(value string) bool {
	return strings.IndexFunc(value, unicode.IsSpace) >= 0
}

func isASCIIUnsignedInteger(value string) bool {
	if value == "" {
		return false
	}
	for _, character := range value {
		if character < '0' || character > '9' {
			return false
		}
	}
	return true
}
