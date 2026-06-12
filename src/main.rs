use std::path::Path;
use micapp::audio;
use micapp::state::Settings;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--list-devices".to_string()) {
        audio::list_devices();
        return;
    }

    if let Some(path) = arg_value(&args, "--save-default-preset") {
        let settings = Settings::default();
        match settings.save(Path::new(&path)) {
            Ok(()) => println!("Default preset saved to {path}"),
            Err(e) => eprintln!("Error: {e}"),
        }
        return;
    }

    let preset = arg_value(&args, "--preset");
    let input  = arg_value(&args, "--input");
    let output = arg_value(&args, "--output");

    if input.is_some() || output.is_some() {
        let settings = match &preset {
            Some(path) => match Settings::load(Path::new(path)) {
                Ok(s) => {
                    println!("Loaded preset: {path}");
                    s
                }
                Err(e) => {
                    eprintln!("Error loading preset: {e}");
                    std::process::exit(1);
                }
            },
            None => Settings::default(),
        };

        audio::start_passthrough(input.as_deref(), output.as_deref(), settings);
        return;
    }

    println!("micapp — real-time microphone processing");
    println!();
    println!("Usage:");
    println!("  micapp --list-devices");
    println!("  micapp --input <name> --output <name>");
    println!("  micapp --input <name> --output <name> --preset voice.toml");
    println!("  micapp --save-default-preset voice.toml");
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}
