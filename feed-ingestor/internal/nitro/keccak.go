package nitro

import (
	"encoding/binary"
	"math/bits"
)

var keccakRoundConstants = [24]uint64{
	0x0000000000000001, 0x0000000000008082,
	0x800000000000808a, 0x8000000080008000,
	0x000000000000808b, 0x0000000080000001,
	0x8000000080008081, 0x8000000000008009,
	0x000000000000008a, 0x0000000000000088,
	0x0000000080008009, 0x000000008000000a,
	0x000000008000808b, 0x800000000000008b,
	0x8000000000008089, 0x8000000000008003,
	0x8000000000008002, 0x8000000000000080,
	0x000000000000800a, 0x800000008000000a,
	0x8000000080008081, 0x8000000000008080,
	0x0000000080000001, 0x8000000080008008,
}

var keccakRotations = [24]int{1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44}
var keccakPi = [24]int{10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1}

func keccak256(input []byte) [32]byte {
	const rate = 136
	var state [25]uint64
	for len(input) >= rate {
		absorbBlock(&state, input[:rate])
		keccakF1600(&state)
		input = input[rate:]
	}

	var block [rate]byte
	copy(block[:], input)
	block[len(input)] = 0x01
	block[rate-1] ^= 0x80
	absorbBlock(&state, block[:])
	keccakF1600(&state)

	var out [32]byte
	for i := 0; i < 4; i++ {
		binary.LittleEndian.PutUint64(out[i*8:], state[i])
	}
	return out
}

func absorbBlock(state *[25]uint64, block []byte) {
	for i := 0; i < len(block)/8; i++ {
		state[i] ^= binary.LittleEndian.Uint64(block[i*8:])
	}
}

func keccakF1600(state *[25]uint64) {
	var bc [5]uint64
	for _, rc := range keccakRoundConstants {
		for i := 0; i < 5; i++ {
			bc[i] = state[i] ^ state[i+5] ^ state[i+10] ^ state[i+15] ^ state[i+20]
		}
		for i := 0; i < 5; i++ {
			t := bc[(i+4)%5] ^ bits.RotateLeft64(bc[(i+1)%5], 1)
			for j := 0; j < 25; j += 5 {
				state[j+i] ^= t
			}
		}

		t := state[1]
		for i := 0; i < 24; i++ {
			j := keccakPi[i]
			bc[0] = state[j]
			state[j] = bits.RotateLeft64(t, keccakRotations[i])
			t = bc[0]
		}

		for j := 0; j < 25; j += 5 {
			for i := 0; i < 5; i++ {
				bc[i] = state[j+i]
			}
			for i := 0; i < 5; i++ {
				state[j+i] ^= (^bc[(i+1)%5]) & bc[(i+2)%5]
			}
		}

		state[0] ^= rc
	}
}
