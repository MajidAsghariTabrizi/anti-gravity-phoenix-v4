package publisher

import (
	"bufio"
	"encoding/json"
	"fmt"
	"net"
	"strings"
	"time"
)

type Publisher interface {
	Publish(subject string, value any) error
	Close() error
}

type NATSCorePublisher struct {
	conn net.Conn
}

func DialNATSCore(addr string, timeout time.Duration) (*NATSCorePublisher, error) {
	if strings.HasPrefix(addr, "nats://") {
		addr = strings.TrimPrefix(addr, "nats://")
	}
	conn, err := net.DialTimeout("tcp", addr, timeout)
	if err != nil {
		return nil, err
	}
	reader := bufio.NewReader(conn)
	if _, err := reader.ReadString('\n'); err != nil {
		conn.Close()
		return nil, err
	}
	if _, err := fmt.Fprintf(conn, "CONNECT {\"verbose\":false,\"pedantic\":true,\"name\":\"phoenix-feed-ingestor\"}\r\n"); err != nil {
		conn.Close()
		return nil, err
	}
	return &NATSCorePublisher{conn: conn}, nil
}

func (p *NATSCorePublisher) Publish(subject string, value any) error {
	payload, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if _, err := fmt.Fprintf(p.conn, "PUB %s %d\r\n", subject, len(payload)); err != nil {
		return err
	}
	if _, err := p.conn.Write(payload); err != nil {
		return err
	}
	_, err = p.conn.Write([]byte("\r\n"))
	return err
}

func (p *NATSCorePublisher) Close() error {
	if p.conn == nil {
		return nil
	}
	return p.conn.Close()
}

type MemoryPublisher struct {
	Messages []PublishedMessage
}

type PublishedMessage struct {
	Subject string
	Value   any
}

func (p *MemoryPublisher) Publish(subject string, value any) error {
	p.Messages = append(p.Messages, PublishedMessage{Subject: subject, Value: value})
	return nil
}

func (p *MemoryPublisher) Close() error { return nil }
