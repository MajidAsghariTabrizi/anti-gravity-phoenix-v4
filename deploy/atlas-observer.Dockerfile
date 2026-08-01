FROM golang:1.23-alpine AS build
WORKDIR /src/atlas-observer
COPY atlas-observer/go.mod atlas-observer/go.sum ./
RUN go mod download
COPY atlas-observer ./
RUN go test ./...
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/atlas-observer ./cmd/atlas-observer

FROM alpine:3.20
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/atlas-observer /usr/local/bin/atlas-observer
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/atlas-observer"]
