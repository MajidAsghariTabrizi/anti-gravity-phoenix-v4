#[test]
#[ignore]
fn bench_decision_path() {
    let start = std::time::Instant::now();
    let mut acc = 0u128;
    for amount in 1u128..=25 {
        acc = acc.saturating_add(amount * amount);
    }
    let elapsed = start.elapsed();
    println!("25 optimizer evaluations fixture accumulator={acc} elapsed_ns={}", elapsed.as_nanos());
}

