//! A generator built from parts, and a property that is false.
//!
//!   cargo run --example orders

use qcheck::{forall, gens};

/// Price in öre for a quantity, with twenty percent off from ten items up.
/// The bug: ten items cost less than nine.
fn total(unit_price: u64, quantity: u64) -> u64 {
    let full = unit_price * quantity;
    if quantity >= 10 {
        full * 8 / 10
    } else {
        full
    }
}

fn main() {
    // An order is a unit price of 1 to 500 kr, in öre, and a quantity.
    let order = gens::zip(gens::range(100u64, 50_000), gens::range(1u64, 100));

    forall(order, |(price, quantity)| {
        total(price, quantity) >= total(price, quantity - 1)
    });
}
