package observer

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"math/rand"
	"net/http"
	"time"

	"github.com/gorilla/websocket"
)

type Client struct {
	ledger *Ledger
	logger *log.Logger
	dialer websocket.Dialer
	sink   AuctionSink
}

// AuctionSink receives only fully decoded, identity-bound auctions after the
// immutable ledger append succeeds. Implementations must be idempotent.
type AuctionSink interface {
	RecordAtlasAuction(context.Context, *LedgerRecord) error
}

type subscriptionRequest struct {
	JSONRPC string   `json:"jsonrpc"`
	ID      uint64   `json:"id"`
	Method  string   `json:"method"`
	Params  []string `json:"params"`
}

type subscriptionResponse struct {
	JSONRPC string `json:"jsonrpc"`
	ID      uint64 `json:"id"`
	Result  string `json:"result"`
	Error   *struct {
		Code    int    `json:"code"`
		Message string `json:"message"`
	} `json:"error"`
}

func NewClient(ledger *Ledger, logger *log.Logger) *Client {
	return NewClientWithSink(ledger, logger, nil)
}

func NewClientWithSink(ledger *Ledger, logger *log.Logger, sink AuctionSink) *Client {
	return &Client{
		ledger: ledger,
		logger: logger,
		sink:   sink,
		dialer: websocket.Dialer{
			HandshakeTimeout: 15 * time.Second,
			ReadBufferSize:   WebSocketBufferBytes,
			WriteBufferSize:  WebSocketBufferBytes,
			Proxy:            http.ProxyFromEnvironment,
		},
	}
}

func SubscriptionPayload() subscriptionRequest {
	return subscriptionRequest{
		JSONRPC: "2.0",
		ID:      1,
		Method:  "solver_subscribe",
		Params:  []string{"userOperations"},
	}
}

func (c *Client) Run(ctx context.Context) error {
	backoff := time.Second
	for {
		complete, err := c.ledger.Complete(time.Now())
		if err != nil {
			return err
		}
		if complete {
			return nil
		}
		if err := c.runConnection(ctx); err == nil {
			backoff = time.Second
		} else if errors.Is(err, context.Canceled) {
			return nil
		} else {
			c.logger.Printf("atlas gateway disconnected: %v", err)
			if err := c.ledger.RecordReconnect(time.Now().UTC()); err != nil {
				return err
			}
			jitter := time.Duration(rand.Int63n(int64(backoff/4 + 1)))
			timer := time.NewTimer(backoff + jitter)
			select {
			case <-ctx.Done():
				timer.Stop()
				return nil
			case <-timer.C:
			}
			if backoff < 30*time.Second {
				backoff *= 2
				if backoff > 30*time.Second {
					backoff = 30 * time.Second
				}
			}
		}
	}
}

func (c *Client) runConnection(ctx context.Context) (returnErr error) {
	conn, response, err := c.dialer.DialContext(ctx, OfficialSearcherGateway, nil)
	if err != nil {
		if response != nil {
			return fmt.Errorf("dial Atlas gateway: HTTP %d", response.StatusCode)
		}
		return fmt.Errorf("dial Atlas gateway: %w", err)
	}
	defer conn.Close()
	connectionClosed := make(chan struct{})
	defer close(connectionClosed)
	go func() {
		select {
		case <-ctx.Done():
			conn.Close()
		case <-connectionClosed:
		}
	}()
	conn.SetReadLimit(WebSocketReadLimitBytes)
	if err := conn.SetReadDeadline(time.Now().Add(90 * time.Second)); err != nil {
		return err
	}
	conn.SetPongHandler(func(string) error {
		return conn.SetReadDeadline(time.Now().Add(90 * time.Second))
	})
	if err := conn.WriteJSON(SubscriptionPayload()); err != nil {
		return fmt.Errorf("subscribe to Atlas gateway: %w", err)
	}
	_, rawAck, err := conn.ReadMessage()
	if err != nil {
		return fmt.Errorf("read Atlas subscription acknowledgement: %w", err)
	}
	var ack subscriptionResponse
	if err := json.Unmarshal(rawAck, &ack); err != nil {
		return fmt.Errorf("decode Atlas subscription acknowledgement: %w", err)
	}
	if ack.JSONRPC != "2.0" || ack.ID != 1 || ack.Error != nil || ack.Result == "" {
		return errors.New("Atlas subscription was not accepted")
	}
	if err := c.ledger.RecordSubscription(time.Now().UTC(), ack.Result); err != nil {
		return err
	}
	defer func() {
		if err := c.ledger.RecordDisconnected(time.Now().UTC()); err != nil && returnErr == nil {
			returnErr = err
		}
	}()
	c.logger.Print("atlas read-only subscription accepted")

	for {
		if complete, err := c.ledger.Complete(time.Now()); err != nil {
			return err
		} else if complete {
			return nil
		}
		if err := conn.SetReadDeadline(time.Now().Add(90 * time.Second)); err != nil {
			return err
		}
		messageType, raw, err := conn.ReadMessage()
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return err
		}
		if messageType != websocket.TextMessage {
			continue
		}
		record, decodeErr := DecodeAndValidateNotification(raw, time.Now().UTC())
		if decodeErr != nil {
			if errors.Is(decodeErr, ErrOtherChain) {
				if err := c.ledger.RecordFilteredOtherChain(time.Now().UTC()); err != nil {
					return err
				}
				continue
			}
			digest := sha256.Sum256(raw)
			invalid := InvalidRecord{
				ObservedAt:         time.Now().UTC(),
				Reason:             decodeErr.Error(),
				NotificationSHA256: hex.EncodeToString(digest[:]),
			}
			if err := c.ledger.AppendInvalid(invalid); err != nil {
				return err
			}
			c.logger.Printf("rejected Atlas notification reason=%s", decodeErr)
			continue
		}
		added, err := c.ledger.Append(record)
		if err != nil {
			return err
		}
		if added {
			if c.sink != nil {
				if err := c.sink.RecordAtlasAuction(ctx, record); err != nil {
					return fmt.Errorf("persist Atlas auction identity: %w", err)
				}
			}
			asset := "unknown"
			if record.OracleUpdate != nil && record.OracleUpdate.Asset != nil {
				asset = *record.OracleUpdate.Asset
			}
			state := c.ledger.Snapshot(time.Now())
			c.logger.Printf("auction recorded asset=%s relevant_aave=%t count=%d", asset, record.RelevantAaveAuction, state.UniqueAuctionCount)
		}
	}
}
