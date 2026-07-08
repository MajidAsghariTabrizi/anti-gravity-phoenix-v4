use phoenix_engine::config::EngineConfig;
use phoenix_engine::execution::{ExecutionCoordinator, ExecutionMode};

fn main() {
    let cfg = EngineConfig::from_env();
    let mode = ExecutionMode::from_env(cfg.mode.as_str(), cfg.live_execution);
    let coordinator = ExecutionCoordinator::new(mode);
    println!(
        "phoenix-engine mode={} live_allowed={}",
        coordinator.mode().as_str(),
        coordinator.live_allowed()
    );
    if std::env::var("PHOENIX_ONESHOT").map(|v| v == "true").unwrap_or(false) {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
