package observer

const (
	Schema                    = "phoenix.atlas-auction.v1"
	StateSchema               = "phoenix.atlas-ledger-state.v1"
	OfficialSearcherGateway   = "wss://svr-bid-endpoint.chain.link/ws/solver"
	ArbitrumChainID           = uint64(42161)
	ArbitrumChainIDHex        = "0xa4b1"
	ArbitrumAtlas             = "0x8ad1aE9D97C79aA68A0a151E83ff3942f68F86C1"
	ArbitrumDappControl       = "0xe15BBa987C002ecc3586e81244517877D294d291"
	AaveV3ArbitrumPool        = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
	ObservedSolverGasLimit    = uint64(6_000_000)
	ReconciliationSchema      = "phoenix.atlas-reconciliation.v1"
	RPCTranscriptSchema       = "phoenix.atlas-rpc-transcript.v1"
	ReconciliationLookback    = uint64(512)
	ReconciliationFinality    = uint64(64)
	WebSocketBufferBytes      = 1024 * 1024
	WebSocketReadLimitBytes   = 16 * 1024 * 1024
	DefaultMaximumAuctions    = uint64(500)
	DefaultMaximumObservation = "72h"
)

type Feed struct {
	Asset      string
	Aggregator string
	Parallel   bool
}

var aaveSVRFeeds = map[string]Feed{
	"0xc1720a8240dbd992d95d6c865a15e490901879b1": {Asset: "AAVE", Aggregator: "0xc1720A8240Dbd992d95D6c865A15e490901879B1"},
	"0xb72359b2dc04ff363e094648df78247c98297c20": {Asset: "ARB", Aggregator: "0xB72359B2dc04Ff363e094648DF78247c98297c20"},
	"0xe7c522c60ba7f1b5e398d2312593713e2b19aeb0": {Asset: "BTC", Aggregator: "0xE7c522c60bA7f1b5E398D2312593713e2B19aeb0", Parallel: true},
	"0xfbe1c9f4297d509b4d0eccbc098df7db29da2918": {Asset: "DAI", Aggregator: "0xFBe1C9F4297d509b4D0ECcbc098df7Db29DA2918"},
	"0xa5e1a36938769cbd5a26f5e19d8fcb379f597c83": {Asset: "ETH", Aggregator: "0xa5E1a36938769cbd5a26f5e19D8FCB379f597c83", Parallel: true},
	"0x333399f03b84678ec22842cd467c8fe089e3ef27": {Asset: "EURC", Aggregator: "0x333399F03B84678Ec22842Cd467c8Fe089E3Ef27"},
	"0x674a6d60637891c63116218c38a9a49be07d21bc": {Asset: "FRAX", Aggregator: "0x674a6D60637891C63116218c38a9a49BE07D21bc"},
	"0x0309c05449070ac1ab244b99955ea5fedeb79e6a": {Asset: "GHO", Aggregator: "0x0309C05449070AC1aB244B99955EA5fEdEB79E6A"},
	"0x4c76f02e484e8ce9b6c2358cf9624babc5531e9e": {Asset: "LINK", Aggregator: "0x4c76F02E484e8ce9B6C2358CF9624BabC5531E9e"},
	"0x16c0e73906cda7ac1f137b0f513a00b84c8f7a4e": {Asset: "USDC", Aggregator: "0x16c0e73906CDa7AC1F137B0F513a00b84c8f7A4E"},
	"0x12b8916e7b6297f31c99e3a8e2bda661f27c676a": {Asset: "USDT", Aggregator: "0x12b8916e7B6297f31C99e3A8e2BDa661f27c676A"},
}
