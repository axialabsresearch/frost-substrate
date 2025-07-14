# FROST Substrate

A Substrate implementation of the FROST (Finality-Reliant Optimised State Transition) protocol for cross-chain state verification.

## Overview

This pallet provides:
- State transition verification
- GRANDPA finality proof generation
- Cross-chain message verification
- Threshold signature support

## Installation

Add this to your runtime's `Cargo.toml`:

```toml
[dependencies]
frost-substrate = { git = "https://github.com/axialabsresearch/frost-substrate" }
```

## Usage

1. Import the pallet in your runtime:
```rust
use frost_substrate;
```

2. Implement the pallet's configuration trait:
```rust
impl frost_substrate::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type FrostHash = Hash;
    type MinConfirmations = ConstU32<10>;
    type MaxProofSize = ConstU32<1024>;
}
```

3. Add the pallet to your `construct_runtime!` macro:
```rust
construct_runtime!(
    pub enum Runtime {
        // ...
        Frost: frost_substrate,
    }
);
```

## License

[Apache-2.0](LICENSE-APACHE)
