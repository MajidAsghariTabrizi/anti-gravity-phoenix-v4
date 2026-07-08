package feed

import (
	"bufio"
	"context"
	"io"
)

type LineSource struct {
	scanner *bufio.Scanner
}

func NewLineSource(r io.Reader) *LineSource {
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 0, 64*1024), 16*1024*1024)
	return &LineSource{scanner: scanner}
}

func (s *LineSource) Next(ctx context.Context) ([]byte, error) {
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	default:
	}
	if !s.scanner.Scan() {
		if err := s.scanner.Err(); err != nil {
			return nil, err
		}
		return nil, io.EOF
	}
	return s.scanner.Bytes(), nil
}
