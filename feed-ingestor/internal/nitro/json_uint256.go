package nitro

import (
	"errors"
	"math/big"
)

const maxUint256DecimalDigits = 78

type jsonUint256 struct {
	value big.Int
}

func (value *jsonUint256) UnmarshalJSON(encoded []byte) error {
	if len(encoded) == 0 || len(encoded) > maxUint256DecimalDigits {
		return errors.New("baseFeeL1 is outside the unsigned 256-bit range")
	}
	for _, character := range encoded {
		if character < '0' || character > '9' {
			return errors.New("baseFeeL1 must be an unsigned decimal JSON integer")
		}
	}

	parsed, ok := new(big.Int).SetString(string(encoded), 10)
	if !ok {
		return errors.New("baseFeeL1 must be an unsigned decimal JSON integer")
	}
	if parsed.BitLen() > 256 {
		return errors.New("baseFeeL1 is outside the unsigned 256-bit range")
	}
	value.value.Set(parsed)
	return nil
}

func (value jsonUint256) MarshalJSON() ([]byte, error) {
	return []byte(value.value.String()), nil
}
