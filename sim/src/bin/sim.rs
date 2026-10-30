//! Run the device's decision layer against a scripted world.
//!
//! This is the tool the firmware is developed with: it answers "what would the
//! device do, and what would that cost" without a board, a controller or a power
//! profiler. What it cannot answer is whether the silicon behaves - see
//! `ARCHITECTURE.md`.

use soil_sensor_sim::energy::{Energy, AA_CAPACITY_MAH};
use soil_sensor_sim::{Script, World};

const DAY_MS: u64 = 86_400_000;

fn main() {
    let mut days = 30.0f64;
    let mut lit_at = 0u64;
    let mut verbose = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--days" => {
                index += 1;
                days = args
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(days);
            }
            "--sit-for" => {
                index += 1;
                let seconds: u64 = args
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                lit_at = seconds * 1_000;
            }
            "--verbose" | "-v" => verbose = true,
            "--help" | "-h" => {
                usage();
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                usage();
                std::process::exit(2);
            }
        }
        index += 1;
    }

    // A probe drying out over the first few days, and a cell discharging over
    // the whole run.
    let mut world = World::new()
        .soil(Script::ramp(1600, 900, 400))
        .battery(Script::ramp(1500, 1150, 400))
        .lit_at(lit_at);

    world.run_until((days * DAY_MS as f64) as u64);

    let energy = Energy::from_world(&world);

    println!("{}", energy.summary());
    println!(
        "{} boots | {} soil reports | {} battery reports | {} reboot-count writes",
        world.boot_count(),
        world.soil_reports().len(),
        world.battery_reports().len(),
        world.reboot_count_writes(),
    );
    println!(
        "{} ms awake per boot on average",
        if world.boot_count() == 0 {
            0
        } else {
            world.awake_ms() / world.boot_count() as u64
        },
    );
    match world
        .battery_estimates()
        .last()
        .map(|estimate| estimate.reading)
    {
        Some(Some(reading)) => {
            let remaining = match reading.time_remaining_s {
                Some(s) => format!("{:.0} days", f64::from(s) / 86_400.0),
                None => "unknown".to_string(),
            };
            println!(
                "battery estimate: {}% left, {remaining} remaining",
                reading.percent
            );
        }
        Some(None) => println!("battery estimate: unknown, no cell"),
        None => {}
    }

    if verbose {
        println!();
        println!("boot  started (s)  wake    awake (ms)  outcome");
        for (n, boot) in world.boots().iter().enumerate() {
            println!(
                "{n:>4}  {:>11}  {:?}  {:>10}  {:?}",
                boot.started_at_ms / 1000,
                boot.wake,
                boot.awake_ms,
                boot.outcome,
            );
        }
    }
}

fn usage() {
    println!(
        "sim - run the device's decision layer against a scripted world

USAGE:
    sim [--days N] [--sit-for SECONDS] [--verbose]

OPTIONS:
    --days N            Simulated days to run (default 30)
    --sit-for SECONDS   Operate as a short-idle device for this long before a
                        controller registers, which is what makes the device
                        eligible to sleep
    --verbose, -v       Print one line per boot
    --help, -h          Show this

The current model puts an awake device at {:.0} mA and a sleeping one at
{:.0} uA, against a {:.0} mAh AA cell. Those are the chip's datasheet figures,
not a measurement of this board.",
        soil_sensor_sim::energy::AWAKE_MA,
        soil_sensor_sim::energy::ASLEEP_UA,
        AA_CAPACITY_MAH,
    );
}
