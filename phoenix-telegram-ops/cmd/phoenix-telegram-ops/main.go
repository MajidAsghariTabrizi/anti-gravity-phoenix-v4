// phoenix-telegram-ops is the Phoenix Telegram Operations reporter.
//
// STRICTLY OPERATIONAL/REPORTING ONLY. It has no write path to Phoenix:
//   - every database statement is a single read-only SELECT;
//   - there is no command that arms, disarms, releases, unpauses, changes
//     caps, mutates the database, submits a blockchain transaction, or
//     changes signer/provider authority;
//   - button presses only switch which panel is rendered.
//
// The bot token is read once from a secret file and is never logged.
package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"strconv"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/alerts"
	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/opsstate"
	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/panels"

	_ "github.com/lib/pq" // postgres driver
)

type config struct {
	dsn             string
	tokenFile       string
	chatID          string
	pollSeconds     int
	refreshSeconds  int
	listenAddr      string
	expectedRelease string
	apiBase         string // overridable for tests
}

func loadConfig() config {
	return config{
		dsn:             os.Getenv("POSTGRES_DSN"),
		tokenFile:       os.Getenv("PHOENIX_TELEGRAM_BOT_TOKEN_FILE"),
		chatID:          os.Getenv("PHOENIX_TELEGRAM_OPS_CHAT_ID"),
		pollSeconds:     envInt("PHOENIX_TELEGRAM_POLL_SECONDS", 5),
		refreshSeconds:  envInt("PHOENIX_TELEGRAM_REFRESH_SECONDS", 30),
		listenAddr:      envOr("PHOENIX_TELEGRAM_LISTEN_ADDR", "0.0.0.0:9750"),
		expectedRelease: os.Getenv("PHOENIX_EXPECTED_RELEASE_SHA"),
		apiBase:         envOr("PHOENIX_TELEGRAM_API_BASE", "https://api.telegram.org"),
	}
}

func envOr(key, def string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return def
}

func envInt(key string, def int) int {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 && n <= 3600 {
			return n
		}
	}
	return def
}

// readToken loads the bot token from its secret file. The value is never
// logged; failures report only the reason class.
func readToken(path string) (string, error) {
	if strings.TrimSpace(path) == "" {
		return "", fmt.Errorf("token file not configured")
	}
	info, err := os.Stat(path)
	if err != nil {
		return "", fmt.Errorf("token file absent")
	}
	if info.Size() <= 0 || info.Size() > 4096 {
		return "", fmt.Errorf("token file size invalid")
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("token file unreadable")
	}
	token := strings.TrimSpace(string(raw))
	if token == "" || len(token) < 20 || strings.ContainsAny(token, "\n\r") {
		return "", fmt.Errorf("token content invalid")
	}
	return token, nil
}

type tgClient struct {
	base   string
	token  string
	client *http.Client
}

type tgResponse struct {
	OK          bool            `json:"ok"`
	Description string          `json:"description"`
	Result      json.RawMessage `json:"result"`
}

func newTGClient(base, token string) *tgClient {
	return &tgClient{base: base, token: token, client: &http.Client{Timeout: 65 * time.Second}}
}

// call performs one bounded Telegram API call with a single transport retry.
func (c *tgClient) call(ctx context.Context, method string, payload []byte) (json.RawMessage, error) {
	url := fmt.Sprintf("%s/bot%s/%s", c.base, c.token, method)
	var lastErr error
	for attempt := 0; attempt < 2; attempt++ {
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, strings.NewReader(string(payload)))
		if err != nil {
			return nil, err
		}
		req.Header.Set("Content-Type", "application/json")
		resp, err := c.client.Do(req)
		if err != nil {
			lastErr = err
			time.Sleep(500 * time.Millisecond)
			continue
		}
		defer resp.Body.Close()
		var tr tgResponse
		if err := json.NewDecoder(resp.Body).Decode(&tr); err != nil {
			return nil, fmt.Errorf("%s: undecodable response", method)
		}
		if !tr.OK {
			return nil, fmt.Errorf("%s: %s", method, tr.Description)
		}
		return tr.Result, nil
	}
	return nil, lastErr
}

type botInfo struct {
	ID       int64  `json:"id"`
	Username string `json:"username"`
	Name     string `json:"first_name"`
}

func (c *tgClient) getMe(ctx context.Context) (*botInfo, error) {
	raw, err := c.call(ctx, "getMe", []byte("{}"))
	if err != nil {
		return nil, err
	}
	var bi botInfo
	if err := json.Unmarshal(raw, &bi); err != nil {
		return nil, err
	}
	return &bi, nil
}

type keyboard struct {
	InlineKeyboard [][]inlineButton `json:"inline_keyboard"`
}

type inlineButton struct {
	Text         string `json:"text"`
	CallbackData string `json:"callback_data"`
}

func buildKeyboard(window string) keyboard {
	rows := make([][]inlineButton, 0, len(panels.KeyboardRows)+1)
	labels := map[panels.PanelKey]string{
		panels.KeySystem: "SYSTEM", panels.KeyPnl: "PNL",
		panels.KeyFunnel: "FUNNEL", panels.KeyLanes: "LANES",
		panels.KeyProviders: "PROVIDERS", panels.KeyGroundTruth: "GROUND TRUTH",
		panels.KeyIncidents: "INCIDENTS", panels.KeyHome: "REFRESH",
	}
	for _, r := range panels.KeyboardRows {
		row := make([]inlineButton, 0, len(r))
		for _, k := range r {
			row = append(row, inlineButton{Text: labels[k], CallbackData: string(k) + "|" + window})
		}
		rows = append(rows, row)
	}
	wrow := make([]inlineButton, 0, len(panels.WindowKeys))
	for _, w := range panels.WindowKeys {
		label := w
		if w == window {
			label = "[" + w + "]"
		}
		wrow = append(wrow, inlineButton{Text: label, CallbackData: "window|" + w})
	}
	rows = append(rows, wrow)
	return keyboard{InlineKeyboard: rows}
}

func (c *tgClient) sendPanel(ctx context.Context, chatID, text, window string) (int, error) {
	body, _ := json.Marshal(map[string]any{
		"chat_id": chatID, "text": text, "reply_markup": buildKeyboard(window),
	})
	raw, err := c.call(ctx, "sendMessage", body)
	if err != nil {
		return 0, err
	}
	var msg struct {
		MessageID int `json:"message_id"`
	}
	if err := json.Unmarshal(raw, &msg); err != nil {
		return 0, err
	}
	return msg.MessageID, nil
}

func (c *tgClient) editPanel(ctx context.Context, chatID string, messageID int, text, window string) error {
	body, _ := json.Marshal(map[string]any{
		"chat_id": chatID, "message_id": messageID, "text": text,
		"reply_markup": buildKeyboard(window),
	})
	_, err := c.call(ctx, "editMessageText", body)
	return err
}

func (c *tgClient) answerCallback(ctx context.Context, id string) {
	body, _ := json.Marshal(map[string]any{"callback_query_id": id})
	_, _ = c.call(ctx, "answerCallbackQuery", body) // best-effort
}

type callbackQuery struct {
	ID      string `json:"id"`
	Data    string `json:"data"`
	Message struct {
		MessageID int `json:"message_id"`
	} `json:"message"`
	From struct {
		ID int64 `json:"id"`
	} `json:"from"`
}

type update struct {
	UpdateID      int64          `json:"update_id"`
	CallbackQuery *callbackQuery `json:"callback_query"`
	Message       *struct {
		Chat struct {
			ID int64 `json:"id"`
		} `json:"chat"`
		Text string `json:"text"`
	} `json:"message"`
}

type app struct {
	cfg          config
	db           *sql.DB
	tg           *tgClient
	homeMsgID    int
	window       string
	lastSnap     *opsstate.Snapshot
	mismatchSeen bool
}

func main() {
	log.SetFlags(log.LstdFlags | log.LUTC)
	cfg := loadConfig()
	mux := http.NewServeMux()
	ready := func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok"))
	}
	mux.HandleFunc("/readyz", ready)
	srv := &http.Server{Addr: cfg.listenAddr, Handler: mux, ReadHeaderTimeout: 3 * time.Second}
	go func() {
		if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Printf("TELEGRAM_OPS_HTTP_EXIT err=%v", err)
		}
	}()

	token, tokErr := readToken(cfg.tokenFile)
	if tokErr != nil {
		log.Printf("TELEGRAM_OPS_DISABLED reason=%s", tokErr.Error())
		select {} // stay alive and healthy-but-disabled; no restart loop
	}
	if cfg.dsn == "" || cfg.chatID == "" {
		log.Printf("TELEGRAM_OPS_DISABLED reason=missing_dsn_or_chat_id dsn_present=%v chat_present=%v",
			cfg.dsn != "", cfg.chatID != "")
		select {}
	}

	db, err := sql.Open("postgres", cfg.dsn+"+sslmode=disable")
	if err != nil {
		log.Printf("TELEGRAM_OPS_FATAL reason=dsn_invalid")
		os.Exit(1)
	}
	db.SetMaxOpenConns(2)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(5 * time.Minute)

	ctx := context.Background()
	tg := newTGClient(cfg.apiBase, token)
	bi, err := tg.getMe(ctx)
	if err != nil {
		log.Printf("TELEGRAM_OPS_FATAL reason=getme_failed class=transport")
		os.Exit(1)
	}
	// Identity metadata only — never the token.
	log.Printf("TELEGRAM_OPS_READY bot_id=%d username=@%s name=%q chat=%s poll=%ds refresh=%ds",
		bi.ID, bi.Username, bi.Name, cfg.chatID, cfg.pollSeconds, cfg.refreshSeconds)

	a := &app{cfg: cfg, db: db, tg: tg, window: "24H"}
	a.lastSnap = a.takeSnapshot(ctx)

	go a.updatesLoop()
	go a.homeLoop()
	a.alertLoop()
}

func (a *app) takeSnapshot(ctx context.Context) *opsstate.Snapshot {
	snapCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	snap, err := opsstate.Take(snapCtx, a.db)
	if err != nil {
		log.Printf("TELEGRAM_OPS_SNAPSHOT_FAILED class=query_error")
		return nil
	}
	snap.ExpectedReleaseSHA = a.cfg.expectedRelease
	snap.MismatchSeen = a.mismatchSeen
	return &snap
}

func (a *app) updatesLoop() {
	offset := int64(0)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 70*time.Second)
		body, _ := json.Marshal(map[string]any{
			"offset": offset + 1, "timeout": 50,
			"allowed_updates": []string{"message", "callback_query"},
		})
		raw, err := a.tg.call(ctx, "getUpdates", body)
		cancel()
		if err != nil {
			time.Sleep(3 * time.Second)
			continue
		}
		var ups []update
		if err := json.Unmarshal(raw, &ups); err != nil {
			continue
		}
		for _, u := range ups {
			if u.UpdateID > offset {
				offset = u.UpdateID
			}
			a.handleUpdate(u)
		}
	}
}

func (a *app) handleUpdate(u update) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	switch {
	case u.CallbackQuery != nil:
		cb := u.CallbackQuery
		if strconv.FormatInt(cb.From.ID, 10) != a.cfg.chatID {
			a.tg.answerCallback(ctx, cb.ID)
			return // owner-only control surface
		}
		window := a.window
		parts := strings.SplitN(cb.Data, "|", 2)
		action := parts[0]
		if len(parts) == 2 && parts[1] != "" {
			window = parts[1]
		}
		switch action {
		case "window":
			a.window = window
			a.renderAndEdit(ctx, cb.Message.MessageID, panels.KeyHome, window)
		default:
			a.renderAndEdit(ctx, cb.Message.MessageID, panels.PanelKey(action), window)
		}
		a.tg.answerCallback(ctx, cb.ID)
	case u.Message != nil && strconv.FormatInt(u.Message.Chat.ID, 10) == a.cfg.chatID:
		msgID, err := a.tg.sendPanel(ctx, a.cfg.chatID, panels.Render(panels.KeyHome, a.window, *a.snapshot()), a.window)
		if err == nil {
			a.homeMsgID = msgID
		}
	}
}

func (a *app) snapshot() *opsstate.Snapshot {
	if a.lastSnap != nil {
		return a.lastSnap
	}
	return a.takeSnapshot(context.Background())
}

func (a *app) renderAndEdit(ctx context.Context, messageID int, key panels.PanelKey, window string) {
	text := panels.Render(key, window, *a.snapshot())
	if err := a.tg.editPanel(ctx, a.cfg.chatID, messageID, text, window); err != nil {
		if !strings.Contains(err.Error(), "message is not modified") {
			log.Printf("TELEGRAM_OPS_PANEL_EDIT_FAILED panel=%s class=transport", key)
		}
	}
}

func (a *app) homeLoop() {
	for {
		time.Sleep(time.Duration(a.cfg.refreshSeconds) * time.Second)
		if a.homeMsgID == 0 {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		snap := a.takeSnapshot(ctx)
		if snap != nil {
			a.lastSnap = snap
			text := panels.Render(panels.KeyHome, a.window, *snap)
			if err := a.tg.editPanel(ctx, a.cfg.chatID, a.homeMsgID, text, a.window); err != nil &&
				!strings.Contains(err.Error(), "message is not modified") {
				log.Printf("TELEGRAM_OPS_HOME_REFRESH_FAILED class=transport")
			}
		}
		cancel()
	}
}

func (a *app) alertLoop() {
	for {
		time.Sleep(time.Duration(a.cfg.pollSeconds) * time.Second)
		cur := a.takeSnapshot(context.Background())
		if cur == nil {
			continue
		}
		found := alerts.Diff(a.lastSnap, *cur)
		if cur.ExpectedReleaseSHA != "" && cur.ReleaseSHA != "" && cur.ExpectedReleaseSHA != cur.ReleaseSHA {
			a.mismatchSeen = true
		}
		a.lastSnap = cur
		for _, al := range found {
			ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			body, _ := json.Marshal(map[string]any{"chat_id": a.cfg.chatID, "text": "⚠ " + al.Text})
			if _, err := a.tg.call(ctx, "sendMessage", body); err != nil {
				log.Printf("TELEGRAM_OPS_ALERT_FAILED key=%s class=transport", al.Key)
			}
			cancel()
		}
	}
}
