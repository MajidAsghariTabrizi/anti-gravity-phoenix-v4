FROM golang:1.23-alpine AS build
WORKDIR /src/atlas-observer
COPY atlas-observer/go.mod atlas-observer/go.sum ./
RUN go mod download
COPY atlas-observer ./
COPY fixtures/replay/aave_profit_path_counterfactual_v1.json /src/fixtures/replay/aave_profit_path_counterfactual_v1.json
RUN go test ./...
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/atlas-observer ./cmd/atlas-observer
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/atlas-aave-hunter ./cmd/atlas-aave-hunter
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/atlas-reconciler ./cmd/atlas-reconciler

FROM alpine:3.20
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /out/atlas-observer /usr/local/bin/atlas-observer
COPY --from=build /out/atlas-aave-hunter /usr/local/bin/atlas-aave-hunter
COPY --from=build /out/atlas-reconciler /usr/local/bin/atlas-reconciler
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/atlas-observer"]
