FROM golang:1.23-alpine AS build
WORKDIR /src/feed-ingestor
COPY feed-ingestor/go.mod ./
COPY feed-ingestor ./
RUN go test ./...
RUN go build -o /out/feed-ingestor ./cmd/feed-ingestor

FROM alpine:3.20
WORKDIR /app
COPY --from=build /out/feed-ingestor /usr/local/bin/feed-ingestor
COPY fixtures ./fixtures
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/feed-ingestor"]

