package main

import "testing"

// Regression guard for the 2026-08-25 production incident: the previous
// implementation appended "+sslmode=disable" to URL-form DSNs, which corrupted
// the database name server-side (FATAL: database
// "phoenix_v5_xxx+sslmode=disable" does not exist) and left the ops bot with a
// permanently failing snapshot query.
func TestWithSSLModeDisable(t *testing.T) {
	cases := []struct {
		name string
		in   string
		want string
	}{
		{
			name: "url_form_without_query",
			in:   "postgres://u:p@postgres:5432/phoenix_v5",
			want: "postgres://u:p@postgres:5432/phoenix_v5?sslmode=disable",
		},
		{
			name: "url_form_keeps_existing_params_sorted",
			in:   "postgres://u@h/db?application_name=x",
			want: "postgres://u@h/db?application_name=x&sslmode=disable",
		},
		{
			name: "keyword_form_appends_space_syntax",
			in:   "host=postgres user=u dbname=d",
			want: "host=postgres user=u dbname=d sslmode=disable",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := withSSLModeDisable(tc.in)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tc.want {
				t.Fatalf("got %q want %q", got, tc.want)
			}
			if got == tc.in+"+sslmode=disable" {
				t.Fatalf("regressed to plus-suffix concatenation")
			}
		})
	}

	if _, err := withSSLModeDisable("not a valid url ://"); err == nil {
		t.Fatalf("expected error for invalid url-form dsn")
	}
}
