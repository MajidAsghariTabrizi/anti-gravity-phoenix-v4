use phoenix_live_executor::executor_rotation::{
    validate_creation_bytecode, validate_plan, RotationPlan,
};
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 || args[1] != "validate" {
        eprintln!("usage: phoenix-executor-rotation validate PLAN_JSON CREATION_BYTECODE");
        return ExitCode::from(2);
    }
    let plan: RotationPlan = match fs::read(&args[2])
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(plan) => plan,
        None => {
            eprintln!("ROTATION_PLAN_INVALID");
            return ExitCode::from(1);
        }
    };
    if validate_plan(&plan).is_err() || validate_creation_bytecode(&plan, &args[3]).is_err() {
        eprintln!("ROTATION_PLAN_INVALID");
        return ExitCode::from(1);
    }
    println!("PHOENIX_EXECUTOR_ROTATION_PLAN_OK");
    println!("schema={}", plan.schema);
    println!("chain_id={}", plan.chain_id);
    println!("old_executor={}", plan.old_executor);
    println!("maximum_input_amount={}", plan.maximum_input_amount);
    ExitCode::SUCCESS
}
