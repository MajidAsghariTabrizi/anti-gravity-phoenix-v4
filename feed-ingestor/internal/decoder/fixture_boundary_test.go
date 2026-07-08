package decoder

import (
	"bufio"
	"os"
	"path/filepath"
	"testing"
)

func TestFixtureBoundariesDecode(t *testing.T) {
	names := []string{
		"profitable.ndjson",
		"non-profitable.ndjson",
		"unsupported-router.ndjson",
		"incomplete-state.ndjson",
	}
	for _, name := range names {
		t.Run(name, func(t *testing.T) {
			d := NewOrderedDecoder(fixedNow)
			lines := readFixtureLines(t, name)
			if len(lines) == 0 {
				t.Fatal("fixture is empty")
			}
			for _, line := range lines {
				if _, err := d.DecodeJSONFrame([]byte(line)); err != nil {
					t.Fatalf("decode %s: %v", name, err)
				}
			}
		})
	}
}

func TestDuplicateFixtureIsDetected(t *testing.T) {
	d := NewOrderedDecoder(fixedNow)
	lines := readFixtureLines(t, "duplicate.ndjson")
	if len(lines) != 2 {
		t.Fatalf("expected two duplicate fixture lines, got %d", len(lines))
	}
	if _, err := d.DecodeJSONFrame([]byte(lines[0])); err != nil {
		t.Fatal(err)
	}
	result, err := d.DecodeJSONFrame([]byte(lines[1]))
	if err != nil {
		t.Fatal(err)
	}
	if !result.Duplicate {
		t.Fatal("expected duplicate fixture to report duplicate sequence")
	}
}

func TestFixtureSequenceGapIsDetected(t *testing.T) {
	d := NewOrderedDecoder(fixedNow)
	first := readFixtureLines(t, "profitable.ndjson")[0]
	third := readFixtureLines(t, "non-profitable.ndjson")[0]
	if _, err := d.DecodeJSONFrame([]byte(first)); err != nil {
		t.Fatal(err)
	}
	result, err := d.DecodeJSONFrame([]byte(third))
	if err != nil {
		t.Fatal(err)
	}
	if !result.Gap || result.GapFrom != 2 || result.GapTo != 2 {
		t.Fatalf("unexpected gap result: %+v", result)
	}
}

func readFixtureLines(t *testing.T, name string) []string {
	t.Helper()
	path := filepath.Join("..", "..", "..", "fixtures", "feed", name)
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	var lines []string
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		if scanner.Text() != "" {
			lines = append(lines, scanner.Text())
		}
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	return lines
}
