# The Telegram operations reporter is a read-only Go sidecar shipped inside the
# dashboard image. It is started only by the live-autonomous overlay service
# `phoenix-telegram-ops`, which overrides this streamlit entrypoint; the
# dashboard service itself never runs it.
FROM golang:1.23-alpine AS telegram-ops-builder
WORKDIR /src/phoenix-telegram-ops
COPY phoenix-telegram-ops/go.mod phoenix-telegram-ops/go.sum ./
RUN go mod download
COPY phoenix-telegram-ops/ ./
RUN CGO_ENABLED=0 go test ./... \
  && CGO_ENABLED=0 go build -trimpath -o /out/phoenix-telegram-ops ./cmd/phoenix-telegram-ops

FROM python:3.12-slim
WORKDIR /app
COPY --from=telegram-ops-builder /out/phoenix-telegram-ops /usr/local/bin/phoenix-telegram-ops
COPY dashboard/requirements.txt ./requirements.txt
RUN pip install --no-cache-dir -r requirements.txt
COPY dashboard/__init__.py dashboard/app.py dashboard/snapshot_model.py ./dashboard/
ENV HOME=/tmp \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
USER 65532:65532
EXPOSE 8501
ENTRYPOINT ["streamlit", "run", "dashboard/app.py", "--server.address=0.0.0.0", "--server.port=8501", "--server.headless=true", "--server.fileWatcherType=none", "--browser.gatherUsageStats=false", "--client.toolbarMode=minimal"]
