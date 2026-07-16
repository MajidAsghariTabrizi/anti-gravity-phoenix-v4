FROM golang:1.23-alpine AS feed-build
WORKDIR /src/feed-ingestor
COPY feed-ingestor/go.mod ./
COPY feed-ingestor ./
COPY fixtures /src/fixtures
RUN go test ./...
RUN go build -o /out/feed-ingestor ./cmd/feed-ingestor

FROM golang:1.23-alpine AS migration-build
WORKDIR /src/migration-runner
COPY migration-runner/go.mod migration-runner/go.sum ./
RUN go mod download
COPY migration-runner ./
COPY migrations /src/migrations
RUN go test ./...
RUN go build -o /out/migration-runner ./cmd/migration-runner

FROM alpine:3.20
WORKDIR /app
COPY --from=feed-build /out/feed-ingestor /usr/local/bin/feed-ingestor
COPY --from=migration-build /out/migration-runner /usr/local/bin/migration-runner
COPY fixtures ./fixtures
COPY migrations ./migrations
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/feed-ingestor"]
